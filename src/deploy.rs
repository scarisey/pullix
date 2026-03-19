use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::{
    config::Config,
    flake::FlakeRef,
    git::{Commit, LatestCommits},
    nix_commands::NixCommands,
};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Deployed {
    Init,
    TestAligned(Commit),
    ProdAligned(Commit),
    TestFailed(Commit),
    ProdFailed(Commit),
}
impl Deployed {
    pub fn test_commit(&self) -> Option<&Commit> {
        match self {
            Deployed::Init | Deployed::ProdAligned(_) | Deployed::ProdFailed(_) => None,
            Deployed::TestAligned(commit) => Some(commit),
            Deployed::TestFailed(commit) => Some(commit),
        }
    }
    pub fn prod_commit(&self) -> Option<&Commit> {
        match self {
            Deployed::Init | Deployed::TestAligned(_) | Deployed::TestFailed(_) => None,
            Deployed::ProdAligned(commit) => Some(commit),
            Deployed::ProdFailed(commit) => Some(commit),
        }
    }

    pub fn failed(&self) -> Deployed {
        match self {
            Deployed::Init => self.clone(),
            Deployed::TestAligned(commit) => Deployed::TestFailed(commit.clone()),
            Deployed::ProdAligned(commit) => Deployed::ProdFailed(commit.clone()),
            Deployed::TestFailed(_) => self.clone(),
            Deployed::ProdFailed(_) => self.clone(),
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Deployments {
    history: Vec<Deployed>,
}

impl Deployments {
    pub fn last_deployment(&self) -> Option<&Deployed> {
        self.history.last()
    }

    pub async fn load_from_path(path: &str) -> anyhow::Result<Self> {
        let data = tokio::fs::read_to_string(path).await;
        let deployments = match data {
            Err(_) => Deployments {
                history: vec![Deployed::Init],
            },
            Ok(serialized) => serde_json::from_str(&serialized)?,
        };
        Ok(deployments)
    }
    pub async fn save_to_path(&mut self, path: &str, keep_last: usize) -> anyhow::Result<()> {
        self.trim_history(keep_last);
        let data = serde_json::to_string_pretty(self)?;
        tokio::fs::write(path, data).await?;
        Ok(())
    }
    pub fn should_deploy(&self, current_commits: &LatestCommits) -> ShouldDeploy {
        match current_commits {
            LatestCommits::Test(commit) => {
                if !self.contains_commit_test(&commit) {
                    ShouldDeploy::ToTest {
                        deployments: self.add_test_deployment(&commit),
                        commit: commit.clone(),
                    }
                } else {
                    ShouldDeploy::Nothing
                }
            }
            LatestCommits::Prod(commit) => {
                if !self.contains_commit_prod(&commit) {
                    ShouldDeploy::ToProd {
                        deployments: self.add_prod_deployment(&commit),
                        commit: commit.clone(),
                    }
                } else {
                    ShouldDeploy::Nothing
                }
            }
            LatestCommits::Distance {
                test,
                prod,
                distance,
            } => {
                if *distance <= 0 && !self.contains_commit_prod(&prod) {
                    ShouldDeploy::ToProd {
                        deployments: self.add_prod_deployment(&prod),
                        commit: prod.clone(),
                    }
                } else if *distance > 0 && !self.contains_commit_test(&test) {
                    ShouldDeploy::ToTest {
                        deployments: self.add_test_deployment(&test),
                        commit: test.clone(),
                    }
                } else {
                    ShouldDeploy::Nothing
                }
            }
        }
    }

    fn add_test_deployment(&self, latest_commits: &Commit) -> Self {
        let mut res = Deployments {
            history: self.history.clone(),
        };
        res.history
            .push(Deployed::TestAligned(latest_commits.clone()));
        res
    }
    fn add_prod_deployment(&self, latest_commits: &Commit) -> Self {
        let mut res = Deployments {
            history: self.history.clone(),
        };
        res.history
            .push(Deployed::ProdAligned(latest_commits.clone()));
        res
    }

    fn contains_commit_test(&self, commit: &Commit) -> bool {
        self.history
            .iter()
            .filter_map(|x| x.test_commit())
            .any(|c| c == commit)
    }

    fn contains_commit_prod(&self, commit: &Commit) -> bool {
        self.history
            .iter()
            .filter_map(|x| x.prod_commit())
            .any(|c| c == commit)
    }

    fn set_last_failed(&mut self) {
        let idx = self.history.len() - 1;
        self.history[idx] = self.history[idx].failed();
    }

    fn trim_history(&mut self, keep_last: usize) {
        if self.history.len() > keep_last {
            self.history.drain(0..self.history.len() - keep_last);
        }
    }
}

#[derive(Debug)]
pub enum ShouldDeploy {
    Nothing,
    ToTest {
        deployments: Deployments,
        commit: Commit,
    },
    ToProd {
        deployments: Deployments,
        commit: Commit,
    },
}

impl ShouldDeploy {
    async fn _run<'a>(
        deployments: &'a mut Deployments,
        flake_ref: &FlakeRef,
        config: &Config,
        nix_commands: &impl NixCommands,
    ) -> Result<Option<&'a Deployed>> {
        if let Err(err) = nix_commands.deploy(flake_ref, &config.hostname).await {
            error!("Error when launching nix command: {}", &err);
            deployments.set_last_failed();
            deployments
                .save_to_path(&config.nixos_state_path(), config.keep_last)
                .await?;
            err.report_error(config).await?;
            Ok(deployments.last_deployment())
        } else {
            deployments
                .save_to_path(&config.nixos_state_path(), config.keep_last)
                .await?;
            let last_deployed = deployments.last_deployment();
            Ok(last_deployed)
        }
    }

    pub fn deployments(&self) -> Option<&Deployments> {
        match self {
            ShouldDeploy::Nothing => None,
            ShouldDeploy::ToTest {
                deployments,
                commit: _,
            } => Some(deployments),
            ShouldDeploy::ToProd {
                deployments,
                commit: _,
            } => Some(deployments),
        }
    }

    pub async fn run(
        &mut self,
        config: &Config,
        nix_commands_for_test: &impl NixCommands,
        nix_commands_for_prod: &impl NixCommands,
    ) -> Result<Option<&Deployed>> {
        let (mut flake_test, mut flake_prod) = FlakeRef::from_config(&config.flake_repo);
        match self {
            ShouldDeploy::Nothing => Ok(None),
            ShouldDeploy::ToTest {
                deployments,
                commit,
            } => {
                flake_test.update_rev(commit);
                ShouldDeploy::_run(deployments, &flake_test, config, nix_commands_for_test).await
            }
            ShouldDeploy::ToProd {
                deployments,
                commit,
            } => {
                flake_prod.update_rev(commit);
                ShouldDeploy::_run(deployments, &flake_prod, config, nix_commands_for_prod).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::{config::tests::make_config, nix_commands::NixCommandError};

    struct NixTestOk;
    impl NixCommands for NixTestOk {
        async fn deploy(
            &self,
            _flake_ref: &FlakeRef,
            _hostname: &str,
        ) -> Result<(), NixCommandError> {
            Ok(())
        }
    }

    struct NixTestKo;
    impl NixCommands for NixTestKo {
        async fn deploy(
            &self,
            _flake_ref: &FlakeRef,
            _hostname: &str,
        ) -> Result<(), NixCommandError> {
            Err(NixCommandError::Execution {
                message: "Example error".into(),
            })
        }
    }

    fn make_latest_commits_prod_last(test_hash: &str, prod_hash: &str) -> LatestCommits {
        LatestCommits::new(Commit::from(test_hash), Commit::from(prod_hash), -1)
    }
    fn make_latest_commits_test_last(test_hash: &str, prod_hash: &str) -> LatestCommits {
        LatestCommits::new(Commit::from(test_hash), Commit::from(prod_hash), 1)
    }

    #[test]
    fn test_should_deploy_empty_history() {
        // Empty history should deploy to prod
        let deployments = Deployments { history: vec![] };
        let current = make_latest_commits_prod_last("test123", "prod123");

        match deployments.should_deploy(&current) {
            ShouldDeploy::ToProd {
                deployments: deps,
                commit,
            } => {
                assert_eq!(deps.history.len(), 1);
                assert_eq!(commit, "prod123".into());
                assert!(matches!(
                    deps.history.last(),
                    Some(Deployed::ProdAligned(_))
                ));
                if let Some(Deployed::ProdAligned(c)) = deps.history.last() {
                    assert_eq!(c, &"prod123".into());
                }
            }
            _ => panic!("Expected ToProd deployment"),
        }
    }

    #[test]
    fn test_should_deploy_after_init() {
        // After Init, should deploy to prod
        let deployments = Deployments {
            history: vec![Deployed::Init],
        };
        let current = make_latest_commits_prod_last("test456", "prod456");

        match deployments.should_deploy(&current) {
            ShouldDeploy::ToProd {
                deployments: deps,
                commit,
            } => {
                assert_eq!(deps.history.len(), 2);
                assert_eq!(commit, "prod456".into());
                assert!(matches!(deps.history[0], Deployed::Init));
                assert!(matches!(
                    deps.history.last(),
                    Some(Deployed::ProdAligned(_))
                ));
                if let Some(Deployed::ProdAligned(c)) = deps.history.last() {
                    assert_eq!(c, &"prod456".into());
                }
            }
            _ => panic!("Expected ToProd deployment"),
        }
    }

    #[test]
    fn test_should_deploy_after_test_aligned() {
        // After TestAligned, should deploy to test if test commit is new
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::TestAligned(Commit::from("test789")),
            ],
        };
        let current = make_latest_commits_test_last("test999", "prod789");

        match deployments.should_deploy(&current) {
            ShouldDeploy::ToTest {
                deployments: deps,
                commit,
            } => {
                assert_eq!(deps.history.len(), 3);
                assert_eq!(commit, "test999".into());
                assert!(matches!(
                    deps.history.last(),
                    Some(Deployed::TestAligned(_))
                ));
                if let Some(Deployed::TestAligned(c)) = deps.history.last() {
                    assert_eq!(c, &"test999".into());
                }
            }
            _ => panic!("Expected ToTest deployment"),
        }
    }

    #[test]
    fn test_should_deploy_after_prod_aligned() {
        // After ProdAligned, should deploy to test first if test commit is new
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::ProdAligned(Commit::from("prod111")),
            ],
        };
        let current = make_latest_commits_test_last("test222", "prod333");

        match deployments.should_deploy(&current) {
            ShouldDeploy::ToTest {
                deployments: deps,
                commit,
            } => {
                assert_eq!(deps.history.len(), 3);
                assert_eq!(commit, "test222".into());
                assert!(matches!(
                    deps.history.last(),
                    Some(Deployed::TestAligned(_))
                ));
                if let Some(Deployed::TestAligned(c)) = deps.history.last() {
                    assert_eq!(c, &"test222".into());
                }
            }
            _ => panic!("Expected ToTest deployment"),
        }
    }

    #[test]
    fn test_should_deploy_complex_history() {
        // Test with a complex deployment history
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::ProdAligned(Commit::from("prod100")),
                Deployed::TestAligned(Commit::from("test200")),
            ],
        };
        let current = make_latest_commits_test_last("test300", "prod300");

        match deployments.should_deploy(&current) {
            ShouldDeploy::ToTest {
                deployments: deps,
                commit,
            } => {
                assert_eq!(deps.history.len(), 4);
                assert_eq!(commit, "test300".into());
                // Verify the history is preserved
                assert!(matches!(deps.history[0], Deployed::Init));
                assert!(matches!(deps.history[1], Deployed::ProdAligned(_)));
                assert!(matches!(deps.history[2], Deployed::TestAligned(_)));
                assert!(matches!(deps.history[3], Deployed::TestAligned(_)));

                if let Some(Deployed::TestAligned(c)) = deps.history.last() {
                    assert_eq!(c, &"test300".into());
                }
            }
            _ => panic!("Expected ToTest deployment"),
        }
    }

    #[test]
    fn test_should_not_deploy_a_previous_failed_commit() {
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::ProdAligned(Commit::from("prod100")),
                Deployed::TestFailed(Commit::from("test200")),
            ],
        };
        let current = make_latest_commits_test_last("test200", "prod100");

        match deployments.should_deploy(&current) {
            ShouldDeploy::Nothing => {}
            _ => panic!("Expected Nothing to deploy"),
        }
    }

    #[test]
    fn test_should_deploy_new_commit_after_failed_commit() {
        // Test with a complex deployment history
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::ProdAligned(Commit::from("prod100")),
                Deployed::TestFailed(Commit::from("test200")),
            ],
        };
        let current = make_latest_commits_test_last("test300", "prod100");

        match deployments.should_deploy(&current) {
            ShouldDeploy::ToTest {
                deployments,
                commit,
            } => {
                assert!(matches!(
                    deployments.last_deployment(),
                    Some(Deployed::TestAligned(_))
                ));
                assert_eq!(commit, Commit::from("test300"));
            }
            _ => panic!("Expected ToTest deployment"),
        }
    }

    #[test]
    fn test_should_deploy_preserves_all_history() {
        // Verify that history is preserved across multiple deployments
        let mut deployments = Deployments {
            history: vec![Deployed::Init],
        };

        let commits1 = make_latest_commits_prod_last("test1", "prod1");
        deployments = match deployments.should_deploy(&commits1) {
            ShouldDeploy::ToProd {
                deployments: deps, ..
            } => deps,
            _ => panic!("Expected ToProd"),
        };
        assert_eq!(deployments.history.len(), 2);

        let commits2 = make_latest_commits_test_last("test2", "prod2");
        deployments = match deployments.should_deploy(&commits2) {
            ShouldDeploy::ToTest {
                deployments: deps, ..
            } => deps,
            _ => panic!("Expected ToTest since test2 is new"),
        };
        assert_eq!(deployments.history.len(), 3);

        // Verify all entries are still there
        assert!(matches!(deployments.history[0], Deployed::Init));
        assert!(matches!(deployments.history[1], Deployed::ProdAligned(_)));
        assert!(matches!(deployments.history[2], Deployed::TestAligned(_)));
    }

    #[test]
    fn test_should_deploy_nothing_when_commits_already_deployed() {
        // Should return Nothing when both commits have been deployed
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::TestAligned(Commit::from("test123")),
                Deployed::ProdAligned(Commit::from("prod123")),
            ],
        };
        let current = make_latest_commits_prod_last("test123", "prod123");

        match deployments.should_deploy(&current) {
            ShouldDeploy::Nothing => {
                // Expected behavior
            }
            _ => panic!("Expected Nothing since both commits are already deployed"),
        }
    }

    #[test]
    fn test_should_deploy_to_prod_when_test_deployed_but_prod_new() {
        // Should deploy to prod when test is already deployed but prod is new
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::TestAligned(Commit::from("test123")),
            ],
        };
        let current = make_latest_commits_prod_last("test123", "prod456");

        match deployments.should_deploy(&current) {
            ShouldDeploy::ToProd {
                deployments: deps,
                commit,
            } => {
                assert_eq!(commit, "prod456".into());
                assert_eq!(deps.history.len(), 3);
                assert!(matches!(
                    deps.history.last(),
                    Some(Deployed::ProdAligned(_))
                ));
            }
            _ => panic!("Expected ToProd since prod commit is new"),
        }
    }

    #[tokio::test]
    async fn test_run_deploy_to_test_save_deployments_when_nix_command_is_ok() {
        let temp_dir = &TempDir::with_prefix("test_pullix").unwrap();
        let config = &make_config(temp_dir.path().to_str().unwrap());
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::TestAligned(Commit::from("test123")),
            ],
        };
        let current = make_latest_commits_prod_last("test123", "prod456");
        let mut should_deploy = deployments.should_deploy(&current);
        let last_deployed = should_deploy.run(&config, &NixTestOk, &NixTestOk).await;

        assert!(matches!(last_deployed, Ok(Some(Deployed::ProdAligned(_)))));

        let state = tokio::fs::read_to_string(config.nixos_state_path())
            .await
            .unwrap();
        let state_deployments: Deployments = serde_json::from_str(&state).unwrap();
        let actual_last_deployment = state_deployments.last_deployment();
        assert!(
            matches!(actual_last_deployment, Some(Deployed::ProdAligned(_))),
            "Last deployment persisted is `{state}`"
        );
    }

    #[tokio::test]
    async fn test_run_deploy_to_test_save_deployments_when_nix_command_is_ko() {
        let _ = tracing_subscriber::fmt::try_init();
        let temp_dir = &TempDir::with_prefix("test_pullix").unwrap();
        let config = &make_config(temp_dir.path().to_str().unwrap());
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::TestAligned(Commit::from("test123")),
            ],
        };
        let current = make_latest_commits_prod_last("test123", "prod456");
        let mut should_deploy = deployments.should_deploy(&current);
        let last_deployed = should_deploy.run(&config, &NixTestOk, &NixTestKo).await;

        assert!(
            matches!(last_deployed, Ok(Some(Deployed::ProdFailed(_)))),
            "last_deployed is {last_deployed:?}"
        );

        let state = tokio::fs::read_to_string(config.nixos_state_path())
            .await
            .unwrap();
        let state_deployments: Deployments = serde_json::from_str(&state).unwrap();
        assert!(
            matches!(
                state_deployments.last_deployment(),
                Some(Deployed::ProdFailed(_))
            ),
            "Last deployment persisted is `{state}`"
        );
    }
    #[tokio::test]
    async fn test_run_deploy_to_test_report_error_when_nix_command_is_ko() {
        let _ = tracing_subscriber::fmt::try_init();
        let temp_dir = &TempDir::with_prefix("test_pullix").unwrap();
        let config = &make_config(temp_dir.path().to_str().unwrap());
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::TestAligned(Commit::from("test123")),
            ],
        };
        let current = make_latest_commits_prod_last("test123", "prod456");
        let mut should_deploy = deployments.should_deploy(&current);

        // Run the first failed deployment
        let last_deployed = should_deploy.run(&config, &NixTestKo, &NixTestKo).await;
        assert!(
            matches!(last_deployed, Ok(Some(Deployed::ProdFailed(_)))),
            "last_deployed is {last_deployed:?}"
        );

        // Run the second failed deployment to trigger the creation of a timestamped report
        let last_deployed = should_deploy.run(&config, &NixTestKo, &NixTestKo).await;
        assert!(
            matches!(last_deployed, Ok(Some(Deployed::ProdFailed(_)))),
            "last_deployed is {last_deployed:?}"
        );

        // Assert that last_report.json exists
        let last_report_path = format!("{}/last_report.json", config.app_dir);
        assert!(
            tokio::fs::try_exists(&last_report_path).await.unwrap(),
            "last_report.json should exist after a failed deployment"
        );

        // Assert that a timestamped report file exists
        let mut has_timestamped_report = false;
        let mut read_dir = tokio::fs::read_dir(&config.app_dir).await.unwrap();
        while let Some(entry) = read_dir.next_entry().await.unwrap() {
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();
            if file_name_str.starts_with(char::is_numeric)
                && file_name_str.ends_with("_report.json")
            {
                has_timestamped_report = true;
                break;
            }
        }
        assert!(
            has_timestamped_report,
            "a timestamped error report should exist after a failed deployment"
        );
    }

    #[tokio::test]
    async fn test_run_deploy_to_test_save_dont_save_deployments_with_last_failed_when_nix_command_is_ko()
     {
        let temp_dir = &TempDir::with_prefix("test_pullix").unwrap();
        let config = &make_config(temp_dir.path().to_str().unwrap());
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::ProdAligned(Commit::from("prod456")),
                Deployed::TestFailed(Commit::from("test123")),
            ],
        };
        let current = make_latest_commits_prod_last("test123", "prod456");
        let mut should_deploy = deployments.should_deploy(&current);
        let last_deployed = should_deploy.run(&config, &NixTestOk, &NixTestKo).await;

        assert!(matches!(last_deployed, Ok(None)));

        let state_exists = tokio::fs::try_exists(config.nixos_state_path()).await;

        assert!(matches!(state_exists, Ok(false)));
    }

    #[test]
    fn test_should_deploy_to_prod_when_test_deployed_and_prod_rebased_on_test() {
        // Should deploy to prod when test is already deployed and prod is rebased on same test commit
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::TestAligned(Commit::from("test123")),
            ],
        };
        let current = LatestCommits::new(Commit::from("test123"), Commit::from("test123"), 0);

        match deployments.should_deploy(&current) {
            ShouldDeploy::ToProd {
                deployments: deps,
                commit,
            } => {
                assert_eq!(commit, "test123".into());
                assert_eq!(deps.history.len(), 3);
                assert!(matches!(
                    deps.history.last(),
                    Some(Deployed::ProdAligned(_))
                ));
            }
            debug_data => panic!(
                "Expected ToProd since prod commit not deployed yet. Actual is {:?}",
                debug_data
            ),
        }
    }

    #[test]
    fn test_should_deploy_nothing_when_prod_deployed_and_test_rebased_on_prod() {
        let deployments = Deployments {
            history: vec![
                Deployed::Init,
                Deployed::ProdAligned(Commit::from("prod123")),
            ],
        };
        let current = LatestCommits::new(Commit::from("prod123"), Commit::from("prod123"), 0);

        match deployments.should_deploy(&current) {
            ShouldDeploy::Nothing => {}
            debug_data => panic!(
                "Expected Nothing since prod commit deployed and test is rebased. Actual is {:?}",
                debug_data
            ),
        }
    }
}
