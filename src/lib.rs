#![feature(once_cell_try)]

use std::time::Duration;

use crate::{
    config::Config,
    git::Git,
    metrics::{LastCommitMetric, RemoteStateMetric},
    nix_commands::NixCommands,
    systemd::ServiceHandler,
};
use anyhow::Result;
use tokio::time::sleep;
use tracing::{Level, debug, span};

pub mod config;
pub mod deploy;
pub mod flake;
pub mod git;
pub mod metrics;
pub mod nix_commands;
pub mod systemd;

pub async fn run_pullix(
    config: &Config,
    git: &Git,
    nix_commands_for_test: &impl NixCommands,
    nix_commands_for_prod: &impl NixCommands,
    service_handler: &impl ServiceHandler,
    last_commit_metric: LastCommitMetric,
    remote_state: RemoteStateMetric,
) -> Result<()> {
    let mut elapsed_secs = config.poll_interval_secs;
    loop {
        let _loop = span!(Level::TRACE, "loop");
        let _loop_span = _loop.enter();
        debug!("Enter loop");
        let delay = config.poll_interval_secs.saturating_sub(elapsed_secs);
        sleep(Duration::from_secs(delay)).await;
        let start_time = std::time::Instant::now();
        debug!("Imposed delay");

        let deployments = deploy::Deployments::load_from_path(&config.actual_state_path()).await?;

        let current_commits = git.sync_and_get_commits(config).await?;
        debug!("Commits loaded {:?}", current_commits);
        let last_commit_on_main = git.last_commit_on_main().await;
        if let Some(commit) = last_commit_on_main {
            debug!("Last commit on main: {:?}", commit);
            remote_state.set(
                &commit,
                current_commits.get_prod(),
                current_commits.get_test(),
            );
        }

        debug!("Deployments loaded");
        if let Some(deployed) = deployments.last_deployment() {
            debug!("Last deployment was: {:?}", deployed);
            last_commit_metric.set(deployed)
        }
        let mut next_action = deployments.should_deploy(&current_commits);
        debug!("Next action for {:?}", next_action);
        next_action
            .run(config, nix_commands_for_test, nix_commands_for_prod, service_handler)
            .await?;

        if let Some(deployed) = next_action.deployments().and_then(|x| x.last_deployment()) {
            last_commit_metric.set(deployed);
        };
        elapsed_secs = start_time.elapsed().as_secs();
    }
}

#[cfg(test)]
mod tests {
    use git2::Repository;
    use opentelemetry::global;
    use tempfile::TempDir;
    use tokio::sync::mpsc::{Receiver, Sender};

    use crate::config::{ConfigFlake, HomeManagerConfig, UrlSpecConfig};
    use crate::flake::{FlakeRef, FlakeType};
    use crate::nix_commands::NixCommandError;
    use crate::metrics::DeploymentType;

    use super::*;
    use crate::config::tests::make_config;
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::sync::Arc;

    // ---------------------------------------------------------------
    //  Test repository helper
    // ---------------------------------------------------------------

    /// A local git repository used as the "remote" for pullix during tests.
    ///
    /// Provides incremental helpers to add commits and create / move tags
    /// so that each test can build exactly the history it needs.
    struct TestRepo {
        repo: Repository,
        dir: tempfile::TempDir,
        /// Monotonic counter used to generate unique file names for commits.
        commit_count: usize,
    }

    impl TestRepo {
        /// Create a new repository with `main` as the default branch and one
        /// initial commit (so that HEAD is valid).
        fn new() -> Result<Self> {
            let repo_dir = tempfile::tempdir()?;

            let opts = &mut git2::RepositoryInitOptions::new();
            opts.initial_head("main");
            let repo = git2::Repository::init_opts(repo_dir.path(), opts)?;

            let mut cfg = repo.config()?;
            cfg.set_str("user.name", "Test User")?;
            cfg.set_str("user.email", "test@example.com")?;
            drop(cfg);

            // Initial commit so HEAD is valid
            let mut index = repo.index()?;
            let init_path = repo_dir.path().join("init.txt");
            fs::write(&init_path, "init")?;
            index.add_path(std::path::Path::new("init.txt"))?;
            index.write()?;
            let tree_oid = index.write_tree()?;
            let tree = repo.find_tree(tree_oid)?;
            let sig = git2::Signature::now("Test User", "test@example.com")?;
            repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])?;
            drop(tree);

            Ok(Self {
                repo,
                dir: repo_dir,
                commit_count: 0,
            })
        }

        /// Filesystem path of the repository (used as the flake repo URL).
        fn path(&self) -> &str {
            self.dir.path().to_str().unwrap()
        }

        /// Append a new commit on `main` (updating HEAD) and return its OID.
        fn add_commit(&mut self) -> Result<git2::Oid> {
            self.commit_count += 1;
            let filename = format!("file_{}.txt", self.commit_count);
            let file_path = self.dir.path().join(&filename);
            fs::write(&file_path, format!("content {}", self.commit_count))?;

            let mut index = self.repo.index()?;
            index.add_path(std::path::Path::new(&filename))?;
            index.write()?;
            let tree_oid = index.write_tree()?;
            let tree = self.repo.find_tree(tree_oid)?;
            let sig = git2::Signature::now("Test User", "test@example.com")?;

            let parent_oid = self.repo.head()?.peel_to_commit()?.id();
            let parent = self.repo.find_commit(parent_oid)?;

            let oid = self.repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                &format!("Commit {}", self.commit_count),
                &tree,
                &[&parent],
            )?;
            Ok(oid)
        }

        /// Create an annotated tag pointing at `commit_oid`.
        fn add_tag(&self, name: &str, commit_oid: git2::Oid) -> Result<git2::Oid> {
            let commit = self.repo.find_commit(commit_oid)?;
            let sig = git2::Signature::now("Test User", "test@example.com")?;
            let tag_oid = self.repo.tag(
                name,
                commit.as_object(),
                &sig,
                &format!("{name} tag"),
                false,
            )?;
            Ok(tag_oid)
        }

        /// Move an existing annotated tag to a different commit.
        ///
        /// Deletes the old tag and creates a new annotated tag with the same
        /// name pointing at `new_commit_oid`.
        fn move_tag(&self, name: &str, new_commit_oid: git2::Oid) -> Result<git2::Oid> {
            self.repo.tag_delete(name)?;
            self.add_tag(name, new_commit_oid)
        }
    }

    // ---------------------------------------------------------------
    //  Mock NixCommands
    // ---------------------------------------------------------------

    /// Mock `NixCommands` implementation that can be toggled between success
    /// and failure at runtime.
    ///
    /// When `should_succeed` is `true`, each call to `deploy` sends the
    /// deployed `FlakeRef` on an internal channel and returns `Ok`.
    /// When `false`, it returns an error without touching the channel.
    ///
    /// Use `NixTestTogglable::new(true)` for an always-succeeding mock and
    /// `NixTestTogglable::new(false)` for an always-failing mock, or call
    /// [`set_should_succeed`](Self::set_should_succeed) between ticks to
    /// change the behaviour mid-test.
    struct NixTestTogglable {
        pub deployments: RefCell<Receiver<FlakeRef>>,
        tx: Sender<FlakeRef>,
        should_succeed: Cell<bool>,
    }
    impl NixTestTogglable {
        pub fn new(initial_success: bool) -> Self {
            let (tx, rx) = tokio::sync::mpsc::channel::<FlakeRef>(10);
            Self {
                deployments: RefCell::new(rx),
                tx,
                should_succeed: Cell::new(initial_success),
            }
        }
        pub fn set_should_succeed(&self, val: bool) {
            self.should_succeed.set(val);
        }
    }
    impl NixCommands for NixTestTogglable {
        async fn deploy(
            &self,
            flake_ref: &FlakeRef,
            _hostname: &str,
        ) -> Result<(), NixCommandError> {
            if self.should_succeed.get() {
                debug!(
                    "Togglable deploy OK for {:?} ({:?})",
                    flake_ref.rev, flake_ref.ref_
                );
                let _ = self
                    .tx
                    .send(flake_ref.clone())
                    .await
                    .map_err(NixCommandError::to_execution)?;
                Ok(())
            } else {
                debug!(
                    "Togglable deploy FAIL for {:?} ({:?})",
                    flake_ref.rev, flake_ref.ref_
                );
                Err(NixCommandError::Execution {
                    message: "Simulated deployment failure".into(),
                })
            }
        }
    }

    // ---------------------------------------------------------------
    //  Mock ServiceHandler
    // ---------------------------------------------------------------

    struct MockServiceHandler;

    impl ServiceHandler for MockServiceHandler {
        fn is_service_changed(&self, _service_name: &str) -> Result<bool> {
            Ok(false)
        }

        fn restart_self(&self) -> Result<()> {
            Ok(())
        }
    }

    // ---------------------------------------------------------------
    //  Shared helpers
    // ---------------------------------------------------------------

    /// Build a `Config` that points at a local git repo and resolves the
    /// `test` / `prod` tags via the `ref_` field (reference-name lookup).
    fn make_pullix_config(repo_path: &str, app_dir: &TempDir) -> Config {
        let mut config = make_config(app_dir.path().to_str().unwrap());
        config.flake_repo = ConfigFlake {
            type_: FlakeType::GitFile,
            repo: repo_path.to_string(),
            host: None,
            test_spec: Some(UrlSpecConfig {
                ref_: Some("test".into()),
                rev: None,
            }),
            prod_spec: Some(UrlSpecConfig {
                ref_: Some("prod".into()),
                rev: None,
            }),
        };
        config.poll_interval_secs = 60;
        config
    }

    /// Build a Home manager `Config` that points at a local git repo and resolves the
    /// `test` / `prod` tags via the `ref_` field (reference-name lookup).
    fn make_home_manager_pullix_config(repo_path: &str, app_dir: &TempDir) -> Config {
        Config {
            flake_repo: ConfigFlake {
                type_: FlakeType::GitFile,
                repo: repo_path.to_string(),
                host: None,
                test_spec: Some(UrlSpecConfig {
                    ref_: Some("test".into()),
                    rev: None,
                }),
                prod_spec: Some(UrlSpecConfig {
                    ref_: Some("prod".into()),
                    rev: None,
                }),
            },
            poll_interval_secs: 60,
            app_dir: app_dir.path().to_str().unwrap().into(),
            hostname: "foo".to_string(),
            otel_http_endpoint: None,
            private_key: None,
            keep_last: 100,
            github_token: None,
            home_manager: Some(HomeManagerConfig {
                username: "foo".into(),
                package: "".into(),
            }),
        }
    }

    /// Advance the paused clock by one full poll interval (+ yield) so that
    /// the `run_pullix` loop completes exactly one iteration.
    async fn advance_one_tick() {
        tokio::time::advance(Duration::from_secs(120)).await;
        tokio::task::yield_now().await;
    }

    /// Wait for a deployment to arrive on the given channel and return the
    /// `FlakeRef` that was deployed.  Panics if the channel is closed.
    async fn expect_deployment(
        nix: &Arc<NixTestTogglable>,
        label: &str,
        expected_commit: &str,
    ) -> FlakeRef {
        debug!("Waiting for {} deployment ...", label);
        let deployed = nix
            .deployments
            .borrow_mut()
            .recv()
            .await
            .unwrap_or_else(|| panic!("Expected a {} deployment but channel closed", label));
        debug!(
            "Received on {} deployment for {:?} ({:?})",
            label, deployed.rev, deployed.ref_
        );
        assert_eq!(
            deployed.rev.as_deref(),
            Some(expected_commit),
            "Expected deployment of commit {} but got {:?}",
            expected_commit,
            deployed.rev
        );
        deployed
    }

    // ---------------------------------------------------------------
    //  Tests
    // ---------------------------------------------------------------

    /// **Test 1 – Start with prod tag, then add test tag**
    ///
    /// History evolution:
    /// ```text
    /// t=0   C0 → C1(prod)                      [main at C1]
    /// t=60  poll → deploy prod
    /// t=60  C0 → C1(prod) → C2(test)            [main at C2]
    /// t=180 poll → deploy test
    /// ```
    #[tokio::test(start_paused = true)]
    pub async fn test_start_with_prod_then_test() {
        let _ = tracing_subscriber::fmt::try_init();
        debug!("test_start_with_prod_then_test");

        let app_dir = TempDir::new().unwrap();
        let mut repo = TestRepo::new().unwrap();

        // C1 tagged "prod"
        let c1 = repo.add_commit().unwrap();
        repo.add_tag("prod", c1).unwrap();

        let config = make_pullix_config(repo.path(), &app_dir);
        let git = Git::default();
        let meter = global::meter("pullix");
        let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
        let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);
        let nix_test = Arc::new(NixTestTogglable::new(true));
        let nix_prod = Arc::new(NixTestTogglable::new(true));
        let ref_test = nix_test.clone();
        let ref_prod = nix_prod.clone();

        let task_local = tokio::task::LocalSet::new();
        task_local
            .run_until(async move {
                let pullix_running = tokio::task::spawn_local(async move {
                    run_pullix(
                        &config,
                        &git,
                        ref_test.as_ref(),
                        ref_prod.as_ref(),
                        &MockServiceHandler,
                        last_commit_metric,
                        remote_state,
                    )
                    .await
                    .unwrap()
                });

                // First tick → only prod tag exists → deploy prod
                tokio::time::advance(Duration::from_secs(60)).await;
                tokio::task::yield_now().await;
                expect_deployment(&nix_prod, "prod", &c1.to_string()).await;

                // Add C2 tagged "test" (ahead of prod)
                let c2 = repo.add_commit().unwrap();
                repo.add_tag("test", c2).unwrap();

                // Second tick → Distance(test=C2, prod=C1, d=1) → deploy test
                advance_one_tick().await;
                expect_deployment(&nix_test, "test", &c2.to_string()).await;

                pullix_running.abort();
            })
            .await;
    }

    /// **Test 1 again – Same test with home manager configuration**
    ///
    /// History evolution:
    /// ```text
    /// t=0   C0 → C1(prod)                      [main at C1]
    /// t=60  poll → deploy prod
    /// t=60  C0 → C1(prod) → C2(test)            [main at C2]
    /// t=180 poll → deploy test
    /// ```
    #[tokio::test(start_paused = true)]
    pub async fn test_hm_config_start_with_prod_then_test() {
        let _ = tracing_subscriber::fmt::try_init();
        debug!("test_start_with_prod_then_test");

        let app_dir = TempDir::new().unwrap();
        let mut repo = TestRepo::new().unwrap();

        // C1 tagged "prod"
        let c1 = repo.add_commit().unwrap();
        repo.add_tag("prod", c1).unwrap();

        let config = make_home_manager_pullix_config(repo.path(), &app_dir);
        let git = Git::default();
        let meter = global::meter("pullix");
        let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
        let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);
        let nix_test = Arc::new(NixTestTogglable::new(true));
        let nix_prod = Arc::new(NixTestTogglable::new(true));
        let ref_test = nix_test.clone();
        let ref_prod = nix_prod.clone();

        let task_local = tokio::task::LocalSet::new();
        task_local
            .run_until(async move {
                let pullix_running = tokio::task::spawn_local(async move {
                    run_pullix(
                        &config,
                        &git,
                        ref_test.as_ref(),
                        ref_prod.as_ref(),
                        &MockServiceHandler,
                        last_commit_metric,
                        remote_state,
                    )
                    .await
                    .unwrap()
                });

                // First tick → only prod tag exists → deploy prod
                tokio::time::advance(Duration::from_secs(60)).await;
                tokio::task::yield_now().await;
                expect_deployment(&nix_prod, "prod", &c1.to_string()).await;

                // Add C2 tagged "test" (ahead of prod)
                let c2 = repo.add_commit().unwrap();
                repo.add_tag("test", c2).unwrap();

                // Second tick → Distance(test=C2, prod=C1, d=1) → deploy test
                advance_one_tick().await;
                expect_deployment(&nix_test, "test", &c2.to_string()).await;

                pullix_running.abort();
            })
            .await;
    }

    /// **Test 2 – Start with test tag, then add prod tag**
    ///
    /// History evolution:
    /// ```text
    /// t=0   C0 → C1(test)                       [main at C1]
    /// t=60  poll → deploy test
    /// t=60  C0 → C1(test) → C2(prod)             [main at C2]
    /// t=180 poll → Distance(test=C1, prod=C2, d=-1) → deploy prod
    /// ```
    #[tokio::test(start_paused = true)]
    pub async fn test_start_with_test_then_prod() {
        let _ = tracing_subscriber::fmt::try_init();
        debug!("test_start_with_test_then_prod");

        let app_dir = TempDir::new().unwrap();
        let mut repo = TestRepo::new().unwrap();

        // C1 tagged "test"
        let c1 = repo.add_commit().unwrap();
        repo.add_tag("test", c1).unwrap();

        let config = make_pullix_config(repo.path(), &app_dir);
        let git = Git::default();
        let meter = global::meter("pullix");
        let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
        let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);
        let nix_test = Arc::new(NixTestTogglable::new(true));
        let nix_prod = Arc::new(NixTestTogglable::new(true));
        let ref_test = nix_test.clone();
        let ref_prod = nix_prod.clone();

        let task_local = tokio::task::LocalSet::new();
        task_local
            .run_until(async move {
                let pullix_running = tokio::task::spawn_local(async move {
                    run_pullix(
                        &config,
                        &git,
                        ref_test.as_ref(),
                        ref_prod.as_ref(),
                        &MockServiceHandler,
                        last_commit_metric,
                        remote_state,
                    )
                    .await
                    .unwrap()
                });

                // First tick → only test tag exists → deploy test
                tokio::time::advance(Duration::from_secs(60)).await;
                tokio::task::yield_now().await;
                expect_deployment(&nix_test, "test", &c1.to_string()).await;

                // Add C2 tagged "prod" (ahead of test on main)
                let c2 = repo.add_commit().unwrap();
                repo.add_tag("prod", c2).unwrap();

                // Second tick → Distance(test=C1, prod=C2, d=-1) → deploy prod
                advance_one_tick().await;
                expect_deployment(&nix_prod, "prod", &c2.to_string()).await;

                pullix_running.abort();
            })
            .await;
    }

    /// **Test 3 – Start with no tags, add prod then test while running**
    ///
    /// The repo initially has commits but zero tags.  Tags are created
    /// while pullix is sleeping between polls.
    ///
    /// History evolution:
    /// ```text
    /// t=0   C0 → C1                              [main at C1, no tags]
    ///       (add prod tag on C1 before first poll fires)
    /// t=60  poll → Prod(C1) → deploy prod
    /// t=60  C0 → C1(prod) → C2(test)              [main at C2]
    /// t=180 poll → Distance(test=C2, prod=C1, d=1) → deploy test
    /// ```
    #[tokio::test(start_paused = true)]
    pub async fn test_start_with_no_tags() {
        let _ = tracing_subscriber::fmt::try_init();
        debug!("test_start_with_no_tags");

        let app_dir = TempDir::new().unwrap();
        let mut repo = TestRepo::new().unwrap();

        // C1 – no tags yet
        let c1 = repo.add_commit().unwrap();

        let config = make_pullix_config(repo.path(), &app_dir);
        let git = Git::default();
        let meter = global::meter("pullix");
        let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
        let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);
        let nix_test = Arc::new(NixTestTogglable::new(true));
        let nix_prod = Arc::new(NixTestTogglable::new(true));
        let ref_test = nix_test.clone();
        let ref_prod = nix_prod.clone();

        let task_local = tokio::task::LocalSet::new();
        task_local
            .run_until(async move {
                let pullix_running = tokio::task::spawn_local(async move {
                    run_pullix(
                        &config,
                        &git,
                        ref_test.as_ref(),
                        ref_prod.as_ref(),
                        &MockServiceHandler,
                        last_commit_metric,
                        remote_state,
                    )
                    .await
                    .unwrap()
                });

                // Add prod tag on C1 *before* the first poll fires
                repo.add_tag("prod", c1).unwrap();

                // First tick → Prod(C1) → deploy prod
                tokio::time::advance(Duration::from_secs(60)).await;
                tokio::task::yield_now().await;
                expect_deployment(&nix_prod, "prod", &c1.to_string()).await;

                // Add C2 tagged "test" (ahead of prod)
                let c2 = repo.add_commit().unwrap();
                repo.add_tag("test", c2).unwrap();

                // Second tick → Distance(test=C2, prod=C1, d=1) → deploy test
                advance_one_tick().await;
                expect_deployment(&nix_test, "test", &c2.to_string()).await;

                pullix_running.abort();
            })
            .await;
    }

    /// **Test 4 – Start with prod, add test, then move prod tag forward**
    ///
    /// After both prod and test have been deployed, the prod tag is moved
    /// to a newer commit (ahead of test).  Pullix should detect the
    /// updated tag on the next poll and deploy the new prod commit.
    ///
    /// History evolution:
    /// ```text
    /// t=0    C0 → C1(prod)                                     [main at C1]
    /// t=60   poll → deploy prod
    /// t=60   C0 → C1(prod) → C2(test)                          [main at C2]
    /// t=180  poll → deploy test
    /// t=180  C0 → C1 → C2(test) → C3(prod moved here)          [main at C3]
    /// t=300  poll → Distance(test=C2, prod=C3, d=-1) → deploy prod(C3)
    /// ```
    #[tokio::test(start_paused = true)]
    pub async fn test_start_with_prod_then_test_then_update_prod() {
        let _ = tracing_subscriber::fmt::try_init();
        debug!("test_start_with_prod_then_test_then_update_prod");

        let app_dir = TempDir::new().unwrap();
        let mut repo = TestRepo::new().unwrap();

        // C1 tagged "prod"
        let c1 = repo.add_commit().unwrap();
        repo.add_tag("prod", c1).unwrap();

        let config = make_pullix_config(repo.path(), &app_dir);
        let git = Git::default();
        let meter = global::meter("pullix");
        let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
        let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);
        let nix_test = Arc::new(NixTestTogglable::new(true));
        let nix_prod = Arc::new(NixTestTogglable::new(true));
        let ref_test = nix_test.clone();
        let ref_prod = nix_prod.clone();

        let task_local = tokio::task::LocalSet::new();
        task_local
            .run_until(async move {
                let pullix_running = tokio::task::spawn_local(async move {
                    run_pullix(
                        &config,
                        &git,
                        ref_test.as_ref(),
                        ref_prod.as_ref(),
                        &MockServiceHandler,
                        last_commit_metric,
                        remote_state,
                    )
                    .await
                    .unwrap()
                });

                // First tick → only prod tag → deploy prod
                tokio::time::advance(Duration::from_secs(60)).await;
                tokio::task::yield_now().await;
                expect_deployment(&nix_prod, "prod (initial)", &c1.to_string()).await;

                // Add C2 tagged "test" (ahead of prod)
                let c2 = repo.add_commit().unwrap();
                repo.add_tag("test", c2).unwrap();

                // Second tick → deploy test
                advance_one_tick().await;
                expect_deployment(&nix_test, "test", &c2.to_string()).await;

                // Move prod tag to C3 (now ahead of test)
                let c3 = repo.add_commit().unwrap();
                repo.move_tag("prod", c3).unwrap();

                // Third tick → sync_repo picks up the moved tag →
                // Distance(test=C2, prod=C3, d=-1) → deploy prod(C3)
                advance_one_tick().await;
                let deployed =
                    expect_deployment(&nix_prod, "prod (updated)", &c3.to_string()).await;
                assert_eq!(
                    deployed.rev.as_deref(),
                    Some(c3.to_string()).as_deref(),
                    "prod deployment should use the updated commit"
                );

                pullix_running.abort();
            })
            .await;
    }

    /// **Test 5 – Failed test deployment is not retried, updated test tag succeeds**
    ///
    /// After deploying a prod tag, a test tag is created but its deployment
    /// fails.  On the next poll the same commit must NOT be retried.  Then
    /// the test tag is moved to a new commit and the deployment succeeds.
    ///
    /// History evolution:
    /// ```text
    /// t=0    C0 → C1(prod)                                     [main at C1]
    /// t=60   poll → deploy prod (OK)
    /// t=60   C0 → C1(prod) → C2(test)                          [main at C2]
    ///        (nix_test set to FAIL)
    /// t=180  poll → deploy test(C2) → FAIL
    /// t=300  poll → C2 already in history (failed) → Nothing
    /// t=300  Move test tag → C3, nix_test set to OK
    /// t=420  poll → test(C3) is new → deploy test(C3) → OK
    /// ```
    #[tokio::test(start_paused = true)]
    pub async fn test_failed_test_not_retried_then_updated_test_succeeds() {
        let _ = tracing_subscriber::fmt::try_init();
        debug!("test_failed_test_not_retried_then_updated_test_succeeds");

        let app_dir = TempDir::new().unwrap();
        let mut repo = TestRepo::new().unwrap();

        // C1 tagged "prod"
        let c1 = repo.add_commit().unwrap();
        repo.add_tag("prod", c1).unwrap();

        let config = make_pullix_config(repo.path(), &app_dir);
        let git = Git::default();
        let meter = global::meter("pullix");
        let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
        let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);

        // Test channel uses the togglable mock (starts as FAIL)
        let nix_test = Arc::new(NixTestTogglable::new(false));
        let nix_prod = Arc::new(NixTestTogglable::new(true));
        let ref_test = nix_test.clone();
        let ref_prod = nix_prod.clone();

        let task_local = tokio::task::LocalSet::new();
        task_local
            .run_until(async move {
                let pullix_running = tokio::task::spawn_local(async move {
                    run_pullix(
                        &config,
                        &git,
                        ref_test.as_ref(),
                        ref_prod.as_ref(),
                        &MockServiceHandler,
                        last_commit_metric,
                        remote_state,
                    )
                    .await
                    .unwrap()
                });

                // ── Tick 1 → only prod tag → deploy prod (OK) ──
                tokio::time::advance(Duration::from_secs(60)).await;
                tokio::task::yield_now().await;
                expect_deployment(&nix_prod, "prod", &c1.to_string()).await;

                // Add C2 tagged "test" (ahead of prod).
                // nix_test is still set to FAIL.
                let c2 = repo.add_commit().unwrap();
                repo.add_tag("test", c2).unwrap();

                // ── Tick 2 → deploy test(C2) → FAIL ──
                advance_one_tick().await;
                // The deployment failed, so nothing should appear on the
                // success channel.
                assert!(
                    nix_test.deployments.borrow_mut().try_recv().is_err(),
                    "test deployment should have failed – nothing on channel"
                );

                // ── Tick 3 → C2 is already recorded (failed) → Nothing ──
                advance_one_tick().await;
                assert!(
                    nix_test.deployments.borrow_mut().try_recv().is_err(),
                    "failed commit should not be retried"
                );

                // Move test tag to C3 and switch mock to succeed
                let c3 = repo.add_commit().unwrap();
                repo.move_tag("test", c3).unwrap();
                nix_test.set_should_succeed(true);

                // ── Tick 4 → test(C3) is new → deploy test(C3) → OK ──
                advance_one_tick().await;
                let deployed = nix_test
                    .deployments
                    .borrow_mut()
                    .recv()
                    .await
                    .expect("Expected a successful test deployment for C3");
                assert_eq!(
                    deployed.rev.as_deref(),
                    Some(c3.to_string()).as_deref(),
                    "test deployment should use the updated commit C3"
                );

                pullix_running.abort();
            })
            .await;
    }

    /// **Test 6 – Test deployed, then prod rebased on same commit → deploy prod**
    ///
    /// After deploying a test tag, the prod tag is created on the same
    /// commit.  Since prod was never deployed, pullix should deploy prod.
    ///
    /// History evolution:
    /// ```text
    /// t=0    C0 → C1(test)                                     [main at C1]
    /// t=60   poll → only test tag → deploy test
    /// t=60   Add prod tag on C1 → C0 → C1(test, prod)
    /// t=180  poll → Distance(test=C1, prod=C1, d=0) → deploy prod(C1)
    /// ```
    #[tokio::test(start_paused = true)]
    pub async fn test_deploy_to_prod_when_test_deployed_and_prod_rebased_on_test() {
        let _ = tracing_subscriber::fmt::try_init();
        debug!("test_deploy_to_prod_when_test_deployed_and_prod_rebased_on_test");

        let app_dir = TempDir::new().unwrap();
        let mut repo = TestRepo::new().unwrap();

        // C1 tagged "test"
        let c1 = repo.add_commit().unwrap();
        repo.add_tag("test", c1).unwrap();

        let config = make_pullix_config(repo.path(), &app_dir);
        let git = Git::default();
        let meter = global::meter("pullix");
        let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
        let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);
        let nix_test = Arc::new(NixTestTogglable::new(true));
        let nix_prod = Arc::new(NixTestTogglable::new(true));
        let ref_test = nix_test.clone();
        let ref_prod = nix_prod.clone();

        let task_local = tokio::task::LocalSet::new();
        task_local
            .run_until(async move {
                let pullix_running = tokio::task::spawn_local(async move {
                    run_pullix(
                        &config,
                        &git,
                        ref_test.as_ref(),
                        ref_prod.as_ref(),
                        &MockServiceHandler,
                        last_commit_metric,
                        remote_state,
                    )
                    .await
                    .unwrap()
                });

                // First tick → only test tag exists → deploy test
                tokio::time::advance(Duration::from_secs(60)).await;
                tokio::task::yield_now().await;
                expect_deployment(&nix_test, "test", &c1.to_string()).await;

                // Add prod tag on the SAME commit C1 (prod rebased on test)
                repo.add_tag("prod", c1).unwrap();

                // Second tick → Distance(test=C1, prod=C1, d=0) →
                // distance <= 0 and prod not yet deployed → deploy prod
                advance_one_tick().await;
                let deployed =
                    expect_deployment(&nix_prod, "prod (rebased on test)", &c1.to_string()).await;
                assert_eq!(
                    deployed.rev.as_deref(),
                    Some(c1.to_string()).as_deref(),
                    "prod deployment should use the same commit as test"
                );

                pullix_running.abort();
            })
            .await;
    }

    /// **Test 7 – Prod deployed, then test rebased on same commit → deploy nothing**
    ///
    /// After deploying a prod tag, the test tag is created on the same
    /// commit.  Since prod is already deployed and test points to the
    /// same commit (distance = 0), pullix should do nothing.
    ///
    /// History evolution:
    /// ```text
    /// t=0    C0 → C1(prod)                                     [main at C1]
    /// t=60   poll → only prod tag → deploy prod
    /// t=60   Add test tag on C1 → C0 → C1(prod, test)
    /// t=180  poll → Distance(test=C1, prod=C1, d=0) → Nothing
    /// ```
    #[tokio::test(start_paused = true)]
    pub async fn test_deploy_nothing_when_prod_deployed_and_test_rebased_on_prod() {
        let _ = tracing_subscriber::fmt::try_init();
        debug!("test_deploy_nothing_when_prod_deployed_and_test_rebased_on_prod");

        let app_dir = TempDir::new().unwrap();
        let mut repo = TestRepo::new().unwrap();

        // C1 tagged "prod"
        let c1 = repo.add_commit().unwrap();
        repo.add_tag("prod", c1).unwrap();

        let config = make_pullix_config(repo.path(), &app_dir);
        let git = Git::default();
        let meter = global::meter("pullix");
        let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
        let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);
        let nix_test = Arc::new(NixTestTogglable::new(true));
        let nix_prod = Arc::new(NixTestTogglable::new(true));
        let ref_test = nix_test.clone();
        let ref_prod = nix_prod.clone();

        let task_local = tokio::task::LocalSet::new();
        task_local
            .run_until(async move {
                let pullix_running = tokio::task::spawn_local(async move {
                    run_pullix(
                        &config,
                        &git,
                        ref_test.as_ref(),
                        ref_prod.as_ref(),
                        &MockServiceHandler,
                        last_commit_metric,
                        remote_state,
                    )
                    .await
                    .unwrap()
                });

                // First tick → only prod tag exists → deploy prod
                tokio::time::advance(Duration::from_secs(60)).await;
                tokio::task::yield_now().await;
                expect_deployment(&nix_prod, "prod", &c1.to_string()).await;

                // Add test tag on the SAME commit C1 (test rebased on prod)
                repo.add_tag("test", c1).unwrap();

                // Second tick → Distance(test=C1, prod=C1, d=0) →
                // distance <= 0 but prod already deployed → Nothing
                advance_one_tick().await;
                assert!(
                    nix_test.deployments.borrow_mut().try_recv().is_err(),
                    "test should not be deployed when rebased on already-deployed prod"
                );
                assert!(
                    nix_prod.deployments.borrow_mut().try_recv().is_err(),
                    "prod should not be redeployed"
                );

                pullix_running.abort();
            })
            .await;
    }
}
