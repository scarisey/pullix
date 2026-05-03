use anyhow::{Context, Result};
use std::process::Command;
use tracing::debug;

pub trait ServiceHandler {
    /// Check if the systemd service has changed by querying NeedDaemonReload property
    fn is_service_changed(&self, service_name: &str) -> Result<bool>;

    /// Restart pullix by replacing the current process with a new one using exec
    /// This uses syscall.Exec equivalent (std::os::unix::process::CommandExt::exec)
    fn restart_self(&self) -> Result<()>;
}

pub struct SystemdServiceHandler;
pub struct SystemdUserServiceHandler;

impl ServiceHandler for SystemdServiceHandler {
    fn is_service_changed(&self, service_name: &str) -> Result<bool> {
        is_service_changed(service_name, false)
    }

    fn restart_self(&self) -> Result<()> {
        restart_self()
    }
}
impl ServiceHandler for SystemdUserServiceHandler {
    fn is_service_changed(&self, service_name: &str) -> Result<bool> {
        is_service_changed(service_name, true)
    }

    fn restart_self(&self) -> Result<()> {
        restart_self()
    }
}

fn is_service_changed(service_name: &str, user: bool) -> Result<bool> {
    let user_args = if user { vec!["--user"] } else { vec![] };
    let output = Command::new("systemctl")
        .args(
            [
                user_args,
                vec!["show", service_name, "--property=NeedDaemonReload"],
            ]
            .concat(),
        )
        .output()
        .context("Failed to run systemctl show command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("systemctl show failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result = stdout.contains("NeedDaemonReload=yes");

    if result {
        debug!(
            "Service {} has changed (NeedDaemonReload=yes)",
            service_name
        );
    }

    Ok(result)
}

fn restart_self() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().context("Failed to get current executable path")?;

    debug!("Restarting pullix using self-exec: {:?}", exe);

    let err = Command::new(&exe)
        .args(std::env::args())
        .env_clear()
        .envs(std::env::vars())
        .exec();

    // exec() should not return on success; if we reach here, something went wrong
    Err(anyhow::anyhow!("Failed to exec new process: {}", err))
}
