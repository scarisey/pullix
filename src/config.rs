use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;

use crate::flake::FlakeType::{self};

#[derive(Debug, Deserialize, Serialize)]
pub struct UrlSpecConfig {
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub rev: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct ConfigFlake {
    #[serde(rename = "type")]
    pub type_: FlakeType,
    pub repo: String,
    pub host: Option<String>, // For custom hosts (e.g., self-hosted GitLab)
    pub prod_spec: Option<UrlSpecConfig>,
    pub test_spec: Option<UrlSpecConfig>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub flake_repo: ConfigFlake,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_app_dir")]
    pub app_dir: String,
    #[serde(default = "linux_hostname")]
    pub hostname: String,
    #[serde(default = "no_prometheus_endpoint")]
    pub otel_http_endpoint: Option<String>,
    #[serde(default = "no_private_key")]
    pub private_key: Option<PrivateKey>,
    pub keep_last: usize,
    #[serde(default = "no_home_manager_switch")]
    pub home_manager_switch: bool,
    pub home_manager_command: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PrivateKey {
    pub path: String,
    pub passphrase_path: String,
    #[serde(skip)]
    passphrase: String,
}
impl PrivateKey {
    fn setup(&mut self) {
        let pass = std::fs::read_to_string(&self.passphrase_path)
            .with_context(|| {
                format!(
                    "Unable to retrieve passphrase for {}",
                    &self.passphrase_path
                )
            })
            .unwrap();
        self.passphrase = pass;
    }

    pub fn passphrase(&self) -> &String {
        &self.passphrase
    }
}

fn no_private_key() -> Option<PrivateKey> {
    None
}
fn no_prometheus_endpoint() -> Option<String> {
    None
}

fn default_poll_interval() -> u64 {
    60 // 1 minute
}

fn default_app_dir() -> String {
    "/var/lib/pullix".to_string()
}

fn linux_hostname() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .trim_end()
        .to_string()
}

fn no_home_manager_switch() -> bool {
    false
}

impl Config {
    pub fn load_from_path(path: &str) -> Result<Self> {
        let contents = &fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path))?;

        let mut config: Config = toml::from_str(contents)
            .with_context(|| format!("Failed to parse config file: {}", path))?;

        if let Some(ref mut pk) = config.private_key {
            pk.setup();
        }

        Ok(config)
    }

    pub fn nixos_state_path(&self) -> String {
        format!("{}/nixos_deployments.json", self.app_dir)
    }
    pub fn home_manager_state_path(&self) -> String {
        format!("{}/home_manager_deployments.json", self.app_dir)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::flake::FlakeType::File;

    pub fn make_config(app_dir: &str) -> Config {
        Config {
            flake_repo: ConfigFlake {
                type_: File,
                repo: "".to_string(),
                host: None,
                test_spec: None,
                prod_spec: None,
            },
            poll_interval_secs: default_poll_interval(),
            app_dir: app_dir.to_string(),
            hostname: "foo".to_string(),
            otel_http_endpoint: no_prometheus_endpoint(),
            private_key: None,
            keep_last: 100,
            home_manager_switch: false,
            home_manager_command: "home-manager".to_string(),
        }
    }
}
