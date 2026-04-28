use std::{cell::OnceCell, fmt::Display, path::Path};

use anyhow::{Context, Result, anyhow};
use git2::Repository;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tracing::debug;

use crate::{
    config::{Config, PrivateKey},
    flake::FlakeRef,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit(String);
impl From<&str> for Commit {
    fn from(value: &str) -> Self {
        Commit(value.to_string())
    }
}
impl From<String> for Commit {
    fn from(value: String) -> Self {
        Commit(value)
    }
}
impl Display for Commit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl TryFrom<&Commit> for git2::Oid {
    type Error = anyhow::Error;

    fn try_from(value: &Commit) -> std::result::Result<Self, Self::Error> {
        git2::Oid::from_str(value.0.as_str())
            .with_context(|| anyhow!("Commit conversion failed on {}", value))
    }
}

#[derive(Debug)]
pub enum LatestCommits {
    Test(Commit),
    Prod(Commit),
    Distance {
        test: Commit,
        prod: Commit,
        distance: isize,
    },
}
impl LatestCommits {
    pub fn new(test: Commit, prod: Commit, distance: isize) -> Self {
        LatestCommits::Distance {
            test,
            prod,
            distance,
        }
    }
    pub fn get_test(&self) -> Option<&Commit> {
        match self {
            LatestCommits::Test(commit) => Some(commit),
            LatestCommits::Prod(_) => None,
            LatestCommits::Distance { test, .. } => Some(test),
        }
    }
    pub fn get_prod(&self) -> Option<&Commit> {
        match self {
            LatestCommits::Test(_) => None,
            LatestCommits::Prod(commit) => Some(commit),
            LatestCommits::Distance { prod, .. } => Some(prod),
        }
    }
}

pub struct Git {
    dir: TempDir,
    repository: OnceCell<Repository>,
}

impl Default for Git {
    fn default() -> Self {
        Self::new().unwrap()
    }
}

impl Git {
    pub fn new() -> Result<Self> {
        let temp_dir = TempDir::with_prefix("pullix-temp-repos")
            .context("Failed to create temporary directory for git operations")?;

        Ok(Self {
            dir: temp_dir,
            repository: OnceCell::new(),
        })
    }

    pub async fn last_commit_on_main(&self) -> Option<Commit> {
        let repo = self.get_repo().ok()?;

        // Try to find the "main" branch first
        if let Ok(reference) = repo.find_reference("refs/heads/main") {
            let commit = reference.peel_to_commit().ok()?;
            return Some(Commit(commit.id().to_string()));
        }

        // Fall back to the "master" branch if "main" is not found
        if let Ok(reference) = repo.find_reference("refs/heads/master") {
            let commit = reference.peel_to_commit().ok()?;
            return Some(Commit(commit.id().to_string()));
        }

        None
    }

    pub async fn sync_and_get_commits(&self, config: &Config) -> Result<LatestCommits> {
        let (test_flake, prod_flake) = FlakeRef::from_config(&config.flake_repo);
        self.sync_repo(&test_flake, config).await?;
        self.sync_repo(&prod_flake, config).await?;
        let latest_commits = self.distance_from(&test_flake, &prod_flake).await?;
        Ok(latest_commits)
    }

    fn get_repo(&self) -> Result<&Repository> {
        self.repository.get_or_try_init(|| -> Result<Repository> {
            let repo = git2::Repository::init(self.dir.path().join("flake_repo"))?;
            Ok(repo)
        })
    }

    async fn sync_repo(&self, flake_ref: &FlakeRef, config: &Config) -> Result<()> {
        debug!("Syncing repo");
        let repo = self.get_repo()?;

        let url = flake_ref.to_git_fetch_url()?;

        debug!("Finding remote");
        // Add or update the remote
        let remote_name = "origin";
        let mut remote = match repo.find_remote(remote_name) {
            Ok(existing_remote) => {
                // Update URL if it changed
                repo.remote_set_url(remote_name, &url)?;
                existing_remote
            }
            Err(_) => {
                // Create new remote
                repo.remote(remote_name, &url)?
            }
        };

        let mut fetch_options = git2::FetchOptions::new();
        let callbacks = Git::credentials_callback(config);
        fetch_options.remote_callbacks(callbacks);

        // Delete all local tags before fetching so that moved or deleted
        // remote tags are always picked up via tag auto-following.
        let local_tags: Vec<String> = repo
            .tag_names(None)?
            .iter()
            .filter_map(|name| name.map(String::from))
            .collect();
        for tag_name in &local_tags {
            repo.tag_delete(tag_name)?;
        }

        debug!("Fetching");
        remote
            .fetch(&["+refs/*:refs/*"], Some(&mut fetch_options), None)
            .with_context(|| format!("Failed to fetch from remote url: {}", &url))?;
        Ok(())
    }

    async fn find_commit(
        &self,
        revision: Option<&str>,
        reference: Option<&str>,
    ) -> Result<Option<Commit>> {
        debug!("Commit search started");
        let repo = self.get_repo()?;
        // Try to find commit by revision (SHA) first
        if let Some(rev) = revision {
            debug!("Searching by revision");
            let oid = git2::Oid::from_str(rev)?;
            let commit = repo.find_commit(oid)?;
            return Ok(Some(Commit(commit.id().to_string())));
        }

        // Try to find commit by reference (branch/tag name)
        if let Some(ref_name) = reference {
            debug!("Searching by reference");
            // Try as a direct reference (e.g., "refs/heads/main")
            if let Ok(reference) = repo.find_reference(ref_name) {
                debug!("Found direct reference");
                let commit = reference.peel_to_commit()?;
                return Ok(Some(Commit(commit.id().to_string())));
            }
            // Try with refs/heads/ prefix (branch)
            let branch_ref = format!("refs/heads/{}", ref_name);
            if let Ok(reference) = repo.find_reference(&branch_ref) {
                debug!("Found branch reference");
                let commit = reference.peel_to_commit()?;
                return Ok(Some(Commit(commit.id().to_string())));
            }
            // Try with refs/tags/ prefix (tag)
            let tag_ref = format!("refs/tags/{}", ref_name);
            if let Ok(reference) = repo.find_reference(&tag_ref) {
                debug!("Found tag reference");
                let commit = reference.peel_to_commit()?;
                return Ok(Some(Commit(commit.id().to_string())));
            }
        }

        // Nothing found
        Ok(None)
    }

    async fn get_distance(&self, a: &Commit, b: &Commit) -> Result<isize> {
        debug!("Getting distance between {} and {}", a, b);
        // Get the main repository that was set up with setup_repo
        let repo = self.get_repo()?;
        let oid_a = git2::Oid::try_from(a)?;
        let oid_b = git2::Oid::try_from(b)?;

        // Same commit → distance is 0
        if oid_a == oid_b {
            return Ok(0);
        }

        let commit_a = repo
            .find_commit(oid_a)
            .with_context(|| format!("Failed to find commit {}", a))?;
        let commit_b = repo
            .find_commit(oid_b)
            .with_context(|| format!("Failed to find commit {}", b))?;

        // Check if a is ancestor of b or vice versa
        let a_is_ancestor_of_b = repo.graph_descendant_of(oid_b, oid_a)?;
        let b_is_ancestor_of_a = repo.graph_descendant_of(oid_a, oid_b)?;

        if a_is_ancestor_of_b {
            // b is ahead of a, return negative distance
            let distance = Self::count_commits_between(repo, &commit_a, &commit_b)?;
            Ok(-(distance as isize))
        } else if b_is_ancestor_of_a {
            // a is ahead of b, return positive distance
            let distance = Self::count_commits_between(repo, &commit_b, &commit_a)?;
            Ok(distance as isize)
        } else {
            // Commits are on diverged branches
            Err(anyhow!(
                "Commits {} and {} are not on the same branch",
                a,
                b
            ))
        }
    }

    async fn distance_from(&self, a: &FlakeRef, b: &FlakeRef) -> Result<LatestCommits> {
        debug!("Commit distance calculation started");
        let commit_a = self
            .find_commit(a.rev.as_deref(), a.ref_.as_deref())
            .await?;
        let commit_b = self
            .find_commit(b.rev.as_deref(), b.ref_.as_deref())
            .await?;
        match (&commit_a, &commit_b) {
            (Some(ca), Some(cb)) => {
                let d = self.get_distance(ca, cb).await?;
                Ok(LatestCommits::new(ca.clone(), cb.clone(), d))
            }
            (Some(ca), None) => Ok(LatestCommits::Test(ca.clone())),
            (None, Some(cb)) => Ok(LatestCommits::Prod(cb.clone())),
            (None, None) => Err(anyhow!("Could not find commits")),
        }
    }

    fn count_commits_between(
        repo: &Repository,
        ancestor: &git2::Commit,
        descendant: &git2::Commit,
    ) -> Result<usize> {
        let mut revwalk = repo.revwalk()?;
        revwalk.push(descendant.id())?;
        revwalk.hide(ancestor.id())?;

        let count = revwalk.count();
        Ok(count)
    }

    fn credentials_callback(config: &Config) -> git2::RemoteCallbacks<'static> {
        let mut callbacks = git2::RemoteCallbacks::new();

        let mut tried_ssh = false;
        let mut tried_userpass = false;
        let pk = config
            .private_key
            .as_ref()
            .map(|pk| (pk.path.clone(), pk.passphrase().clone()));
        let gh_token = config.github_token.clone();

        callbacks.credentials(move |url, username_from_url, allowed_types| {
            // libgit2 may request USERNAME before SSH_KEY during SSH negotiation
            if allowed_types.contains(git2::CredentialType::USERNAME) {
                return git2::Cred::username(username_from_url.unwrap_or("git"));
            }

            if allowed_types.contains(git2::CredentialType::SSH_KEY) && !tried_ssh {
                tried_ssh = true;
                if let Some((path, passphrase)) = pk.as_ref() {
                    return git2::Cred::ssh_key(
                        username_from_url.unwrap_or("git"),
                        None,
                        Path::new(path.as_str()),
                        Some(passphrase.as_str()),
                    );
                }
            }

            // Only offer GitHub token to github.com to avoid leaking it to other hosts
            if allowed_types.contains(git2::CredentialType::USER_PASS_PLAINTEXT)
                && !tried_userpass
                && url.contains("github.com")
            {
                tried_userpass = true;
                if let Some(token) = gh_token.as_ref() {
                    return git2::Cred::userpass_plaintext("x-access-token", token.as_str());
                }
            }

            Err(git2::Error::from_str("No credentials available"))
        });
        callbacks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::make_config as mk_config;
    use crate::config::{ConfigFlake, UrlSpecConfig};
    use crate::flake::FlakeType;
    use std::fs;

    /// Helper: create a git signature
    fn test_sig() -> git2::Signature<'static> {
        git2::Signature::now("Test User", "test@example.com").unwrap()
    }

    /// Helper: add a file and create a commit on top of optional parents
    fn add_commit(
        repo: &Repository,
        dir: &std::path::Path,
        filename: &str,
        content: &str,
        message: &str,
        parents: &[&git2::Commit],
        update_head: bool,
    ) -> Result<git2::Oid> {
        let file_path = dir.join(filename);
        fs::write(&file_path, content)?;
        let mut index = repo.index()?;
        index.add_path(std::path::Path::new(filename))?;
        index.write()?;
        let tree_oid = index.write_tree()?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = test_sig();
        let head_ref = if update_head { Some("HEAD") } else { None };
        let oid = repo.commit(head_ref, &sig, &sig, message, &tree, parents)?;
        Ok(oid)
    }

    /// Helper: initialise a bare-bones repo with `main` as default branch
    fn init_repo(dir: &std::path::Path) -> Result<Repository> {
        let opts = &mut git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = git2::Repository::init_opts(dir, opts)?;
        let mut cfg = repo.config()?;
        cfg.set_str("user.name", "Test User")?;
        cfg.set_str("user.email", "test@example.com")?;
        drop(cfg);
        Ok(repo)
    }

    /// Helper: create an annotated tag on a commit
    fn create_tag(repo: &Repository, name: &str, commit_oid: git2::Oid) -> Result<git2::Oid> {
        let commit = repo.find_commit(commit_oid)?;
        let sig = test_sig();
        let tag_oid = repo.tag(
            name,
            commit.as_object(),
            &sig,
            &format!("{name} tag"),
            false,
        )?;
        Ok(tag_oid)
    }

    /// Build a `Config` that points at a local git repo and looks up
    /// `test` / `prod` tags via the `rev` field (reference name lookup).
    fn make_config(
        repo_path: &str,
        test_spec: Option<UrlSpecConfig>,
        prod_spec: Option<UrlSpecConfig>,
    ) -> (Config, tempfile::TempDir) {
        let app_dir = tempfile::tempdir().unwrap();
        let mut config = mk_config(app_dir.path().to_str().unwrap());
        config.flake_repo = ConfigFlake {
            type_: FlakeType::GitFile,
            repo: repo_path.to_string(),
            host: None,
            test_spec,
            prod_spec,
        };
        (config, app_dir)
    }

    /// Both `test` and `prod` tags on a linear history, test ahead of prod.
    ///
    /// History: C1 → C2(prod) → C3 → C4(test)   [main at C4]
    ///
    /// Expected: `LatestCommits::Distance` with positive distance (test ahead).
    #[tokio::test]
    async fn sync_and_get_commits_both_tags_linear_test_ahead() {
        // -- set up remote repo --
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = init_repo(repo_dir.path()).unwrap();

        let c1 = add_commit(&repo, repo_dir.path(), "f1.txt", "1", "C1", &[], true).unwrap();
        let c1_obj = repo.find_commit(c1).unwrap();

        let c2 = add_commit(
            &repo,
            repo_dir.path(),
            "f2.txt",
            "2",
            "C2",
            &[&c1_obj],
            true,
        )
        .unwrap();
        create_tag(&repo, "prod", c2).unwrap();

        let c2_obj = repo.find_commit(c2).unwrap();
        let c3 = add_commit(
            &repo,
            repo_dir.path(),
            "f3.txt",
            "3",
            "C3",
            &[&c2_obj],
            true,
        )
        .unwrap();
        let c3_obj = repo.find_commit(c3).unwrap();

        let c4 = add_commit(
            &repo,
            repo_dir.path(),
            "f4.txt",
            "4",
            "C4",
            &[&c3_obj],
            true,
        )
        .unwrap();
        create_tag(&repo, "test", c4).unwrap();

        // -- build config --
        let repo_path = repo_dir.path().to_str().unwrap();
        let (config, _app) = make_config(
            repo_path,
            Some(UrlSpecConfig {
                ref_: Some("test".into()),
                rev: None,
            }),
            Some(UrlSpecConfig {
                ref_: Some("prod".into()),
                rev: None,
            }),
        );

        // -- act --
        let git = Git::default();
        let result = git
            .sync_and_get_commits(&config)
            .await
            .expect("sync_and_get_commits failed");

        // -- assert --
        match result {
            LatestCommits::Distance {
                test,
                prod,
                distance,
            } => {
                assert_eq!(test, Commit::from(c4.to_string()), "test commit mismatch");
                assert_eq!(prod, Commit::from(c2.to_string()), "prod commit mismatch");
                assert_eq!(distance, 2, "test should be 2 commits ahead of prod");
            }
            other => panic!("Expected Distance variant, got {:?}", other),
        }

        drop(repo_dir);
    }

    /// Both `test` and `prod` tags on a linear history, prod ahead of test.
    ///
    /// History: C1(test) → C2 → C3(prod)   [main at C3]
    ///
    /// Expected: `LatestCommits::Distance` with negative distance (prod ahead).
    #[tokio::test]
    async fn sync_and_get_commits_both_tags_linear_prod_ahead() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = init_repo(repo_dir.path()).unwrap();

        let c1 = add_commit(&repo, repo_dir.path(), "f1.txt", "1", "C1", &[], true).unwrap();
        create_tag(&repo, "test", c1).unwrap();

        let c1_obj = repo.find_commit(c1).unwrap();
        let c2 = add_commit(
            &repo,
            repo_dir.path(),
            "f2.txt",
            "2",
            "C2",
            &[&c1_obj],
            true,
        )
        .unwrap();
        let c2_obj = repo.find_commit(c2).unwrap();

        let c3 = add_commit(
            &repo,
            repo_dir.path(),
            "f3.txt",
            "3",
            "C3",
            &[&c2_obj],
            true,
        )
        .unwrap();
        create_tag(&repo, "prod", c3).unwrap();

        let repo_path = repo_dir.path().to_str().unwrap();
        let (config, _app) = make_config(
            repo_path,
            Some(UrlSpecConfig {
                ref_: Some("test".into()),
                rev: None,
            }),
            Some(UrlSpecConfig {
                ref_: Some("prod".into()),
                rev: None,
            }),
        );

        let git = Git::default();
        let result = git
            .sync_and_get_commits(&config)
            .await
            .expect("sync_and_get_commits failed");

        match result {
            LatestCommits::Distance {
                test,
                prod,
                distance,
            } => {
                assert_eq!(test, Commit::from(c1.to_string()), "test commit mismatch");
                assert_eq!(prod, Commit::from(c3.to_string()), "prod commit mismatch");
                assert_eq!(distance, -2, "test should be 2 commits behind prod");
            }
            other => panic!("Expected Distance variant, got {:?}", other),
        }

        drop(repo_dir);
    }

    /// Same linear history as `sync_and_get_commits_both_tags_linear_prod_ahead`,
    /// but commits are identified by their SHA (`rev`) on the `main` branch
    /// instead of by tag names (`ref_`). No tags are created.
    ///
    /// History: C1 → C2 → C3   [main at C3]
    ///
    /// Config: test rev = C1 SHA, prod rev = C3 SHA, both on branch main.
    ///
    /// Expected: `LatestCommits::Distance` with negative distance (prod ahead).
    #[tokio::test]
    async fn sync_and_get_commits_revisions_on_main_prod_ahead() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = init_repo(repo_dir.path()).unwrap();

        let c1 = add_commit(&repo, repo_dir.path(), "f1.txt", "1", "C1", &[], true).unwrap();

        let c1_obj = repo.find_commit(c1).unwrap();
        let c2 = add_commit(
            &repo,
            repo_dir.path(),
            "f2.txt",
            "2",
            "C2",
            &[&c1_obj],
            true,
        )
        .unwrap();
        let c2_obj = repo.find_commit(c2).unwrap();

        let c3 = add_commit(
            &repo,
            repo_dir.path(),
            "f3.txt",
            "3",
            "C3",
            &[&c2_obj],
            true,
        )
        .unwrap();

        let repo_path = repo_dir.path().to_str().unwrap();
        let (config, _app) = make_config(
            repo_path,
            Some(UrlSpecConfig {
                ref_: Some("main".into()),
                rev: Some(c1.to_string()),
            }),
            Some(UrlSpecConfig {
                ref_: Some("main".into()),
                rev: Some(c3.to_string()),
            }),
        );

        let git = Git::default();
        let result = git
            .sync_and_get_commits(&config)
            .await
            .expect("sync_and_get_commits failed");

        match result {
            LatestCommits::Distance {
                test,
                prod,
                distance,
            } => {
                assert_eq!(test, Commit::from(c1.to_string()), "test commit mismatch");
                assert_eq!(prod, Commit::from(c3.to_string()), "prod commit mismatch");
                assert_eq!(distance, -2, "test should be 2 commits behind prod");
            }
            other => panic!("Expected Distance variant, got {:?}", other),
        }

        drop(repo_dir);
    }

    /// Both `test` and `prod` tags point to the **same** commit.
    ///
    /// History: C1 → C2(prod, test)   [main at C2]
    ///
    /// Expected: `LatestCommits::Distance` with distance 0.
    #[tokio::test]
    async fn sync_and_get_commits_both_tags_same_commit() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = init_repo(repo_dir.path()).unwrap();

        let c1 = add_commit(&repo, repo_dir.path(), "f1.txt", "1", "C1", &[], true).unwrap();
        let c1_obj = repo.find_commit(c1).unwrap();

        let c2 = add_commit(
            &repo,
            repo_dir.path(),
            "f2.txt",
            "2",
            "C2",
            &[&c1_obj],
            true,
        )
        .unwrap();
        create_tag(&repo, "prod", c2).unwrap();
        create_tag(&repo, "test", c2).unwrap();

        let repo_path = repo_dir.path().to_str().unwrap();
        let (config, _app) = make_config(
            repo_path,
            Some(UrlSpecConfig {
                ref_: Some("test".into()),
                rev: None,
            }),
            Some(UrlSpecConfig {
                ref_: Some("prod".into()),
                rev: None,
            }),
        );

        let git = Git::default();
        let result = git
            .sync_and_get_commits(&config)
            .await
            .expect("sync_and_get_commits failed");

        match result {
            LatestCommits::Distance {
                test,
                prod,
                distance,
            } => {
                assert_eq!(test, Commit::from(c2.to_string()));
                assert_eq!(prod, Commit::from(c2.to_string()));
                assert_eq!(
                    distance, 0,
                    "tags on the same commit should have distance 0"
                );
            }
            other => panic!("Expected Distance variant, got {:?}", other),
        }

        drop(repo_dir);
    }

    /// Only a `test` tag exists (no `prod` tag).
    ///
    /// History: C1 → C2(test)   [main at C2]
    ///
    /// Expected: `LatestCommits::Test`.
    #[tokio::test]
    async fn sync_and_get_commits_only_test_tag() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = init_repo(repo_dir.path()).unwrap();

        let c1 = add_commit(&repo, repo_dir.path(), "f1.txt", "1", "C1", &[], true).unwrap();
        let c1_obj = repo.find_commit(c1).unwrap();

        let c2 = add_commit(
            &repo,
            repo_dir.path(),
            "f2.txt",
            "2",
            "C2",
            &[&c1_obj],
            true,
        )
        .unwrap();
        create_tag(&repo, "test", c2).unwrap();

        let repo_path = repo_dir.path().to_str().unwrap();
        let (config, _app) = make_config(
            repo_path,
            Some(UrlSpecConfig {
                ref_: Some("test".into()),
                rev: None,
            }),
            Some(UrlSpecConfig {
                ref_: Some("prod".into()),
                rev: None,
            }),
        );

        let git = Git::default();
        let result = git
            .sync_and_get_commits(&config)
            .await
            .expect("sync_and_get_commits failed");

        match result {
            LatestCommits::Test(commit) => {
                assert_eq!(commit, Commit::from(c2.to_string()), "test commit mismatch");
            }
            other => panic!("Expected Test variant, got {:?}", other),
        }

        drop(repo_dir);
    }

    /// Only a `prod` tag exists (no `test` tag).
    ///
    /// History: C1 → C2(prod)   [main at C2]
    ///
    /// Expected: `LatestCommits::Prod`.
    #[tokio::test]
    async fn sync_and_get_commits_only_prod_tag() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = init_repo(repo_dir.path()).unwrap();

        let c1 = add_commit(&repo, repo_dir.path(), "f1.txt", "1", "C1", &[], true).unwrap();
        let c1_obj = repo.find_commit(c1).unwrap();

        let c2 = add_commit(
            &repo,
            repo_dir.path(),
            "f2.txt",
            "2",
            "C2",
            &[&c1_obj],
            true,
        )
        .unwrap();
        create_tag(&repo, "prod", c2).unwrap();

        let repo_path = repo_dir.path().to_str().unwrap();
        let (config, _app) = make_config(
            repo_path,
            Some(UrlSpecConfig {
                ref_: Some("test".into()),
                rev: None,
            }),
            Some(UrlSpecConfig {
                ref_: Some("prod".into()),
                rev: None,
            }),
        );

        let git = Git::default();
        let result = git
            .sync_and_get_commits(&config)
            .await
            .expect("sync_and_get_commits failed");

        match result {
            LatestCommits::Prod(commit) => {
                assert_eq!(commit, Commit::from(c2.to_string()), "prod commit mismatch");
            }
            other => panic!("Expected Prod variant, got {:?}", other),
        }

        drop(repo_dir);
    }

    /// Neither `test` nor `prod` tags exist.
    ///
    /// History: C1 → C2   [main at C2, no tags]
    ///
    /// Expected: error ("Could not find commits").
    #[tokio::test]
    async fn sync_and_get_commits_no_tags() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = init_repo(repo_dir.path()).unwrap();

        let c1 = add_commit(&repo, repo_dir.path(), "f1.txt", "1", "C1", &[], true).unwrap();
        let c1_obj = repo.find_commit(c1).unwrap();
        let _c2 = add_commit(
            &repo,
            repo_dir.path(),
            "f2.txt",
            "2",
            "C2",
            &[&c1_obj],
            true,
        )
        .unwrap();

        let repo_path = repo_dir.path().to_str().unwrap();
        let (config, _app) = make_config(
            repo_path,
            Some(UrlSpecConfig {
                ref_: Some("test".into()),
                rev: None,
            }),
            Some(UrlSpecConfig {
                ref_: Some("prod".into()),
                rev: None,
            }),
        );

        let git = Git::default();
        let result = git.sync_and_get_commits(&config).await;

        assert!(result.is_err(), "Expected error when neither tag exists");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Could not find commits"),
            "Unexpected error message: {err_msg}"
        );

        drop(repo_dir);
    }

    /// `test` and `prod` tags are on divergent branches (neither is an
    /// ancestor of the other), but both are reachable from `main` via a
    /// merge commit so that a single fetch brings all objects and tags.
    ///
    /// History:
    ///
    /// ```text
    ///         C2 (prod) ──┐
    ///        /             │
    ///   C1 ─               ├─ C4 (merge, main)
    ///        \             │
    ///         C3 (test) ──┘
    /// ```
    ///
    /// Expected: error ("not on the same branch").
    #[tokio::test]
    async fn sync_and_get_commits_divergent_branches() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = init_repo(repo_dir.path()).unwrap();

        // C1 – root commit on main
        let c1 = add_commit(&repo, repo_dir.path(), "f1.txt", "1", "C1", &[], true).unwrap();
        let c1_obj = repo.find_commit(c1).unwrap();

        // C2 – on a side branch, tagged "prod"
        // (don't update HEAD so main stays at C1 for now)
        let c2 = add_commit(
            &repo,
            repo_dir.path(),
            "f2.txt",
            "2",
            "C2 (prod branch)",
            &[&c1_obj],
            false,
        )
        .unwrap();
        create_tag(&repo, "prod", c2).unwrap();

        // C3 – on main, tagged "test"
        let c3 = add_commit(
            &repo,
            repo_dir.path(),
            "f3.txt",
            "3",
            "C3 (test branch)",
            &[&c1_obj],
            true,
        )
        .unwrap();
        create_tag(&repo, "test", c3).unwrap();

        // C4 – merge commit bringing both branches together on main
        let c2_obj = repo.find_commit(c2).unwrap();
        let c3_obj = repo.find_commit(c3).unwrap();
        let _c4 = add_commit(
            &repo,
            repo_dir.path(),
            "f4.txt",
            "4",
            "C4 (merge)",
            &[&c3_obj, &c2_obj],
            true,
        )
        .unwrap();

        let repo_path = repo_dir.path().to_str().unwrap();
        let (config, _app) = make_config(
            repo_path,
            Some(UrlSpecConfig {
                ref_: Some("test".into()),
                rev: None,
            }),
            Some(UrlSpecConfig {
                ref_: Some("prod".into()),
                rev: None,
            }),
        );

        let git = Git::default();
        let result = git.sync_and_get_commits(&config).await;

        assert!(result.is_err(), "Expected error for diverged branches");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not on the same branch"),
            "Unexpected error message: {err_msg}"
        );

        drop(repo_dir);
    }
}
