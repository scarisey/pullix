use std::fmt::Display;

use crate::{config::Config, flake::FlakeRef, nix_commands};
use anyhow::Result;
use serde::Serialize;
use thiserror::Error;
use tokio::process::Command;
use tracing::debug;

#[derive(Error, Debug, Serialize)]
pub enum NixCommandError {
    #[error("Build failed: {hostname}\nstdout: {stdout}\nstderr: {stderr}")]
    Build {
        hostname: String,
        stderr: String,
        stdout: String,
    },
    #[error("Execution failed: {message}")]
    Execution { message: String },
    #[error("Switch failed: {hostname}\nstdout: {stdout}\nstderr: {stderr}")]
    Switch {
        hostname: String,
        stderr: String,
        stdout: String,
    },
}
impl NixCommandError {
    pub fn to_execution(error: impl Into<Box<dyn std::error::Error>> + Display) -> Self {
        Self::Execution {
            message: error.to_string(),
        }
    }
    pub async fn report_error(&self, config: &Config) -> Result<()> {
        let serialized = serde_json::to_string_pretty(self)?;
        let last_error_path = Self::last_error_path(config);
        if last_error_path.exists() {
            let error_path = Self::error_path(config);
            tokio::fs::rename(&last_error_path, error_path).await?;
        }
        tokio::fs::write(&last_error_path, serialized).await?;
        Ok(())
    }

    fn last_error_path(config: &Config) -> std::path::PathBuf {
        format!("{}/last_report.json", config.app_dir).into()
    }
    fn error_path(config: &Config) -> std::path::PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        format!("{}/{}_report.json", config.app_dir, ts,).into()
    }
}

pub trait NixCommands {
    async fn deploy(&self, flake_ref: &FlakeRef, hostname: &str) -> Result<(), NixCommandError>;
}
async fn deploy(
    flake_ref: &FlakeRef,
    hostname: &str,
    switch_args: &[&str],
) -> Result<(), NixCommandError> {
    let flake_url = flake_ref
        .to_flake_url()
        .map_err(NixCommandError::to_execution)?;
    debug!(
        "Starting {} deployment from {}",
        switch_args.join(" "),
        flake_url
    );

    let build_output = &Command::new("nix")
        .args([
            "build",
            &format!(
                "{}#nixosConfigurations.{}.config.system.build.toplevel",
                &flake_url, hostname
            ),
            "--print-out-paths",
        ])
        .output()
        .await
        .map_err(NixCommandError::to_execution)?;

    if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        let stdout = String::from_utf8_lossy(&build_output.stdout);
        return Err(NixCommandError::Build {
            hostname: hostname.into(),
            stderr: stderr.into(),
            stdout: stdout.into(),
        });
    }

    debug!("Nix build completed successfully, parsing output...");
    let output_parsed = String::from_utf8_lossy(build_output.stdout.trim_ascii());
    let derivation_path = output_parsed
        .lines()
        .rfind(|line| line.starts_with("/nix/store/"))
        .ok_or_else(|| NixCommandError::Execution {
            message: "No derivation path found in build output".into(),
        })?;

    debug!("Built derivation at {}", derivation_path);
    let output = Command::new(format!("{}/bin/switch-to-configuration", derivation_path))
        .args(switch_args)
        .output()
        .await
        .map_err(NixCommandError::to_execution)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(NixCommandError::Switch {
            hostname: hostname.into(),
            stderr: stderr.into(),
            stdout: stdout.into(),
        });
    }

    debug!("Deployment completed successfully");
    Ok(())
}

pub struct Prod;
impl NixCommands for Prod {
    async fn deploy(&self, flake_ref: &FlakeRef, hostname: &str) -> Result<(), NixCommandError> {
        nix_commands::deploy(flake_ref, hostname, &["switch"]).await
    }
}
pub struct Test;
impl NixCommands for Test {
    async fn deploy(&self, flake_ref: &FlakeRef, hostname: &str) -> Result<(), NixCommandError> {
        nix_commands::deploy(flake_ref, hostname, &["test"]).await
    }
}
