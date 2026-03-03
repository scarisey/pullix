use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{config::ConfigFlake, git::Commit};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum FlakeType {
    Git,
    GitHttp,
    GitHttps,
    GitSsh,
    GitFile,
    GitHub,
    GitLab,
    SourceHut,
    Mercurial,
    Tarball,
    File,
    Path,
    Indirect,
}

impl FlakeType {
    #[allow(dead_code)]
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "git" => Ok(FlakeType::Git),
            "git+http" => Ok(FlakeType::GitHttp),
            "git+https" => Ok(FlakeType::GitHttps),
            "git+ssh" => Ok(FlakeType::GitSsh),
            "git+file" => Ok(FlakeType::GitFile),
            "github" => Ok(FlakeType::GitHub),
            "gitlab" => Ok(FlakeType::GitLab),
            "sourcehut" | "git+sourcehut" => Ok(FlakeType::SourceHut),
            "hg" | "hg+http" | "hg+https" | "hg+ssh" | "hg+file" => Ok(FlakeType::Mercurial),
            "tarball" | "tarball+http" | "tarball+https" | "tarball+file" => Ok(FlakeType::Tarball),
            "file" => Ok(FlakeType::File),
            "path" => Ok(FlakeType::Path),
            "indirect" => Ok(FlakeType::Indirect),
            _ => Err(anyhow!("Unknown flake type: {}", s)),
        }
    }
}

impl fmt::Display for FlakeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlakeType::Git => write!(f, "git"),
            FlakeType::GitHttp => write!(f, "git+http"),
            FlakeType::GitHttps => write!(f, "git+https"),
            FlakeType::GitSsh => write!(f, "git+ssh"),
            FlakeType::GitFile => write!(f, "git+file"),
            FlakeType::GitHub => write!(f, "github"),
            FlakeType::GitLab => write!(f, "gitlab"),
            FlakeType::SourceHut => write!(f, "sourcehut"),
            FlakeType::Mercurial => write!(f, "mercurial"),
            FlakeType::Tarball => write!(f, "tarball"),
            FlakeType::File => write!(f, "file"),
            FlakeType::Path => write!(f, "path"),
            FlakeType::Indirect => write!(f, "indirect"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FlakeRef {
    #[serde(rename = "type")]
    pub type_: FlakeType,
    pub repo: String,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub rev: Option<String>,
    pub host: Option<String>, // For custom hosts (e.g., self-hosted GitLab)
}

impl FlakeRef {
    pub fn from_config(config: &ConfigFlake) -> (FlakeRef, FlakeRef) {
        let test = FlakeRef {
            host: config.host.clone(),
            type_: config.type_.clone(),
            repo: config.repo.clone(),
            ref_: config
                .test_spec
                .iter()
                .flat_map(|it| it.ref_.clone())
                .next(),
            rev: config.test_spec.iter().flat_map(|it| it.rev.clone()).next(),
        };
        let prod = FlakeRef {
            host: config.host.clone(),
            type_: config.type_.clone(),
            repo: config.repo.clone(),
            ref_: config
                .prod_spec
                .iter()
                .flat_map(|it| it.ref_.clone())
                .next(),
            rev: config.prod_spec.iter().flat_map(|it| it.rev.clone()).next(),
        };
        (test, prod)
    }

    pub fn update_rev(&mut self, new_rev: &Commit) {
        self.rev = Some(new_rev.to_string());
    }

    pub fn to_git_fetch_url(&self) -> Result<String> {
        match &self.type_ {
            FlakeType::Git
            | FlakeType::GitHttp
            | FlakeType::GitHttps
            | FlakeType::GitSsh
            | FlakeType::GitFile => {
                let url = self.get_base_url()?;
                Ok(url)
            }
            FlakeType::GitHub => {
                let host = self.host.as_deref().unwrap_or("github.com");
                let url = format!("https://{}/{}.git", host, self.repo);
                Ok(url)
            }
            FlakeType::GitLab => {
                let host = self.host.as_deref().unwrap_or("gitlab.com");
                let url = format!("https://{}/{}.git", host, self.repo);
                Ok(url)
            }
            FlakeType::SourceHut => {
                let host = self.host.as_deref().unwrap_or("git.sr.ht");
                let url = format!("https://{}/{}", host, self.repo);
                Ok(url)
            }
            FlakeType::Mercurial => Err(anyhow!(
                "Mercurial repositories are not compatible with git fetch"
            )),
            FlakeType::Tarball | FlakeType::File | FlakeType::Path => Err(anyhow!(format!(
                "{} type is not compatible with git fetch",
                self.type_
            ))),
            FlakeType::Indirect => Err(anyhow!("Indirect flake references must be resolved first")),
        }
    }

    fn get_base_url(&self) -> Result<String> {
        match &self.type_ {
            FlakeType::Git => Ok(self.repo.clone()),
            FlakeType::GitHttp => {
                if self.repo.starts_with("http://") {
                    Ok(self.repo.clone())
                } else {
                    Ok(format!("http://{}", self.repo))
                }
            }
            FlakeType::GitHttps => {
                if self.repo.starts_with("https://") {
                    Ok(self.repo.clone())
                } else {
                    Ok(format!("https://{}", self.repo))
                }
            }
            FlakeType::GitSsh => {
                if self.repo.starts_with("ssh://") || self.repo.contains('@') {
                    Ok(self.repo.clone())
                } else {
                    Ok(format!("ssh://{}", self.repo))
                }
            }
            FlakeType::GitFile => {
                if self.repo.starts_with("file://") {
                    Ok(self.repo.clone())
                } else {
                    Ok(format!("file://{}", self.repo))
                }
            }
            _ => Err(anyhow!("Cannot get base URL for type {}", self.type_)),
        }
    }

    pub fn to_flake_url(&self) -> Result<String> {
        match &self.type_ {
            FlakeType::Git => self.build_flake_url(&format!("git+{}", self.repo)),
            FlakeType::GitHttp => {
                let base = if self.repo.starts_with("http://") {
                    format!("git+{}", self.repo)
                } else {
                    format!("git+http://{}", self.repo)
                };
                self.build_flake_url(&base)
            }
            FlakeType::GitHttps => {
                let base = if self.repo.starts_with("https://") {
                    format!("git+{}", self.repo)
                } else {
                    format!("git+https://{}", self.repo)
                };
                self.build_flake_url(&base)
            }
            FlakeType::GitSsh => {
                let base = if self.repo.starts_with("ssh://") || self.repo.contains('@') {
                    format!("git+ssh://{}", self.repo.trim_start_matches("ssh://"))
                } else {
                    format!("git+ssh://{}", self.repo)
                };
                self.build_flake_url(&base)
            }
            FlakeType::GitFile => {
                // Local git repository - use git+file:// format
                let base = if self.repo.starts_with("file://") {
                    format!("git+{}", self.repo)
                } else if self.repo.starts_with('/') {
                    format!("git+file://{}", self.repo)
                } else {
                    format!("git+file:///{}", self.repo)
                };
                self.build_flake_url(&base)
            }
            FlakeType::GitHub => {
                let host = self.host.as_deref().unwrap_or("github.com");
                let base = format!("github:{}", self.repo);
                // For custom hosts, need to use git+https format
                if self.host.is_some() {
                    let url = format!("git+https://{}/{}.git", host, self.repo);
                    self.build_flake_url(&url)
                } else {
                    self.build_flake_url(&base)
                }
            }
            FlakeType::GitLab => {
                let host = self.host.as_deref().unwrap_or("gitlab.com");
                let base = format!("gitlab:{}", self.repo);
                // For custom hosts, need to use git+https format
                if self.host.is_some() {
                    let url = format!("git+https://{}/{}.git", host, self.repo);
                    self.build_flake_url(&url)
                } else {
                    self.build_flake_url(&base)
                }
            }
            FlakeType::SourceHut => {
                let host = self.host.as_deref().unwrap_or("git.sr.ht");
                let url = format!("git+https://{}/{}", host, self.repo);
                self.build_flake_url(&url)
            }
            FlakeType::Path => {
                // Local path - use path: prefix
                self.build_flake_url(&format!("path:{}", self.repo))
            }
            FlakeType::Mercurial | FlakeType::Tarball | FlakeType::File | FlakeType::Indirect => {
                Err(anyhow!(
                    "{} type is not supported for nixos-rebuild --flake",
                    self.type_
                ))
            }
        }
    }

    /// Helper to build flake URL with ref/rev query parameters
    fn build_flake_url(&self, base: &str) -> Result<String> {
        match (&self.rev, &self.ref_) {
            (Some(rev), _) => {
                // Use rev query parameter for specific commits or tags
                Ok(format!("{}?rev={}", base, rev))
            }
            (None, Some(ref_)) => {
                // Use ref query parameter for branches
                Ok(format!("{}?ref={}", base, ref_))
            }
            (None, None) => {
                // No specific reference
                Ok(base.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // FlakeType tests
    #[test]
    fn test_flake_type_from_str() {
        assert_eq!(FlakeType::from_str("git").unwrap(), FlakeType::Git);
        assert_eq!(FlakeType::from_str("git+http").unwrap(), FlakeType::GitHttp);
        assert_eq!(
            FlakeType::from_str("git+https").unwrap(),
            FlakeType::GitHttps
        );
        assert_eq!(FlakeType::from_str("git+ssh").unwrap(), FlakeType::GitSsh);
        assert_eq!(FlakeType::from_str("git+file").unwrap(), FlakeType::GitFile);
        assert_eq!(FlakeType::from_str("github").unwrap(), FlakeType::GitHub);
        assert_eq!(FlakeType::from_str("gitlab").unwrap(), FlakeType::GitLab);
        assert_eq!(
            FlakeType::from_str("sourcehut").unwrap(),
            FlakeType::SourceHut
        );
        assert_eq!(
            FlakeType::from_str("git+sourcehut").unwrap(),
            FlakeType::SourceHut
        );
        assert_eq!(FlakeType::from_str("hg").unwrap(), FlakeType::Mercurial);
        assert_eq!(FlakeType::from_str("tarball").unwrap(), FlakeType::Tarball);
        assert_eq!(FlakeType::from_str("file").unwrap(), FlakeType::File);
        assert_eq!(FlakeType::from_str("path").unwrap(), FlakeType::Path);
        assert_eq!(
            FlakeType::from_str("indirect").unwrap(),
            FlakeType::Indirect
        );
    }

    #[test]
    fn test_flake_type_from_str_invalid() {
        assert!(FlakeType::from_str("invalid").is_err());
        assert!(FlakeType::from_str("").is_err());
        assert!(FlakeType::from_str("GIT").is_err()); // Case sensitive
    }

    #[test]
    fn test_flake_type_display() {
        assert_eq!(FlakeType::Git.to_string(), "git");
        assert_eq!(FlakeType::GitHub.to_string(), "github");
        assert_eq!(FlakeType::GitSsh.to_string(), "git+ssh");
    }

    #[test]
    fn test_flake_url_github_basic() {
        let flake = FlakeRef {
            type_: FlakeType::GitHub,
            repo: "NixOS/nixpkgs".to_string(),
            ref_: None,
            rev: None,
            host: None,
        };

        assert_eq!(flake.to_flake_url().unwrap(), "github:NixOS/nixpkgs");
    }

    #[test]
    fn test_flake_url_github_with_ref() {
        let flake = FlakeRef {
            type_: FlakeType::GitHub,
            repo: "NixOS/nixpkgs".to_string(),
            ref_: Some("nixos-unstable".to_string()),
            rev: None,
            host: None,
        };

        assert_eq!(
            flake.to_flake_url().unwrap(),
            "github:NixOS/nixpkgs?ref=nixos-unstable"
        );
    }

    #[test]
    fn test_flake_url_github_with_rev() {
        let flake = FlakeRef {
            type_: FlakeType::GitHub,
            repo: "NixOS/nixpkgs".to_string(),
            ref_: None,
            rev: Some("abc123def456".to_string()),
            host: None,
        };

        assert_eq!(
            flake.to_flake_url().unwrap(),
            "github:NixOS/nixpkgs?rev=abc123def456"
        );
    }

    #[test]
    fn test_flake_url_github_custom_host() {
        let flake = FlakeRef {
            type_: FlakeType::GitHub,
            repo: "org/repo".to_string(),
            ref_: Some("main".to_string()),
            rev: None,
            host: Some("github.enterprise.com".to_string()),
        };

        assert_eq!(
            flake.to_flake_url().unwrap(),
            "git+https://github.enterprise.com/org/repo.git?ref=main"
        );
    }

    #[test]
    fn test_flake_url_gitlab_basic() {
        let flake = FlakeRef {
            type_: FlakeType::GitLab,
            repo: "gitlab-org/gitlab".to_string(),
            ref_: None,
            rev: None,
            host: None,
        };

        assert_eq!(flake.to_flake_url().unwrap(), "gitlab:gitlab-org/gitlab");
    }

    #[test]
    fn test_flake_url_gitlab_with_rev() {
        let flake = FlakeRef {
            type_: FlakeType::GitLab,
            repo: "gitlab-org/gitlab".to_string(),
            ref_: None,
            rev: Some("v15.0.0".to_string()),
            host: None,
        };

        assert_eq!(
            flake.to_flake_url().unwrap(),
            "gitlab:gitlab-org/gitlab?rev=v15.0.0"
        );
    }

    #[test]
    fn test_flake_url_git_file_absolute_path() {
        let flake = FlakeRef {
            type_: FlakeType::GitFile,
            repo: "/tmp/test-flake".to_string(),
            ref_: None,
            rev: Some("prod-v1".to_string()),
            host: None,
        };

        assert_eq!(
            flake.to_flake_url().unwrap(),
            "git+file:///tmp/test-flake?rev=prod-v1"
        );
    }

    #[test]
    fn test_flake_url_git_file_with_file_prefix() {
        let flake = FlakeRef {
            type_: FlakeType::GitFile,
            repo: "file:///home/user/repo".to_string(),
            ref_: Some("main".to_string()),
            rev: None,
            host: None,
        };

        assert_eq!(
            flake.to_flake_url().unwrap(),
            "git+file:///home/user/repo?ref=main"
        );
    }

    #[test]
    fn test_flake_url_git_https() {
        let flake = FlakeRef {
            type_: FlakeType::GitHttps,
            repo: "https://git.example.com/repo.git".to_string(),
            ref_: Some("develop".to_string()),
            rev: None,
            host: None,
        };

        assert_eq!(
            flake.to_flake_url().unwrap(),
            "git+https://git.example.com/repo.git?ref=develop"
        );
    }

    #[test]
    fn test_flake_url_git_ssh() {
        let flake = FlakeRef {
            type_: FlakeType::GitSsh,
            repo: "git@github.com:user/repo.git".to_string(),
            ref_: None,
            rev: Some("abc123".to_string()),
            host: None,
        };

        assert_eq!(
            flake.to_flake_url().unwrap(),
            "git+ssh://git@github.com:user/repo.git?rev=abc123"
        );
    }

    #[test]
    fn test_flake_url_path() {
        let flake = FlakeRef {
            type_: FlakeType::Path,
            repo: "/etc/nixos".to_string(),
            ref_: None,
            rev: None,
            host: None,
        };

        assert_eq!(flake.to_flake_url().unwrap(), "path:/etc/nixos");
    }

    #[test]
    fn test_flake_url_sourcehut() {
        let flake = FlakeRef {
            type_: FlakeType::SourceHut,
            repo: "~user/myrepo".to_string(),
            ref_: Some("main".to_string()),
            rev: None,
            host: None,
        };

        assert_eq!(
            flake.to_flake_url().unwrap(),
            "git+https://git.sr.ht/~user/myrepo?ref=main"
        );
    }

    #[test]
    fn test_flake_url_unsupported_types() {
        let mercurial = FlakeRef {
            type_: FlakeType::Mercurial,
            repo: "https://hg.example.com/repo".to_string(),
            ref_: None,
            rev: None,
            host: None,
        };
        assert!(mercurial.to_flake_url().is_err());

        let tarball = FlakeRef {
            type_: FlakeType::Tarball,
            repo: "https://example.com/archive.tar.gz".to_string(),
            ref_: None,
            rev: None,
            host: None,
        };
        assert!(tarball.to_flake_url().is_err());

        let indirect = FlakeRef {
            type_: FlakeType::Indirect,
            repo: "nixpkgs".to_string(),
            ref_: None,
            rev: None,
            host: None,
        };
        assert!(indirect.to_flake_url().is_err());
    }
}
