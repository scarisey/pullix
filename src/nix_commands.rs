use crate::{flake::FlakeRef, nix_commands};
use anyhow::{Context, Result, anyhow};
use tokio::process::Command;
use tracing::debug;

pub trait NixCommands {
    async fn deploy(&self, flake_ref: &FlakeRef, hostname: &str) -> Result<()>;
}
async fn deploy(flake_ref: &FlakeRef, hostname: &str, switch_args: &[&str]) -> Result<()> {
    let flake_url = flake_ref.to_flake_url()?;
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
        .context("Failed to execute nix build")?;

    if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        let stdout = String::from_utf8_lossy(&build_output.stdout);
        return Err(anyhow!(
            "Building {} failed:\nstdout: {}\nstderr: {}",
            hostname,
            stdout,
            stderr
        ));
    }

    debug!("Nix build completed successfully, parsing output...");
    let output_parsed = String::from_utf8_lossy(build_output.stdout.trim_ascii());
    let derivation_path = output_parsed
        .lines()
        .rfind(|line| line.starts_with("/nix/store/"))
        .ok_or_else(|| anyhow!("Failed to find build output path in nix build output"))?;

    debug!("Built derivation at {}", derivation_path);
    let output = Command::new(format!("{}/bin/switch-to-configuration", derivation_path))
        .args(switch_args)
        .output()
        .await
        .context("Failed to execute switch-to-configuration")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(anyhow!(
            "switch-to-configuration failed:\nstdout: {}\nstderr: {}",
            stdout,
            stderr
        ));
    }

    debug!("Deployment completed successfully");
    Ok(())
}

pub struct Prod;
impl NixCommands for Prod {
    async fn deploy(&self, flake_ref: &FlakeRef, hostname: &str) -> Result<()> {
        nix_commands::deploy(flake_ref, hostname, &["switch"]).await
    }
}
pub struct Test;
impl NixCommands for Test {
    async fn deploy(&self, flake_ref: &FlakeRef, hostname: &str) -> Result<()> {
        nix_commands::deploy(flake_ref, hostname, &["test"]).await
    }
}
