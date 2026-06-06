use anyhow::{Context, Result};
use opentelemetry::global;
use pullix::{
    config::Config,
    git::Git,
    observability::{DeploymentType, LastCommitMetric, RemoteStateMetric, setup},
    systemd::{SystemdServiceHandler, SystemdUserServiceHandler},
    webhooks::WebhooksImpl,
    *,
};
use tracing::debug;

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::var("PULLIX_CONFIG").context("Can't find PULLIX_CONFIG env var")?;
    let config = Config::load_from_path(&config_path)
        .with_context(|| format!("Failed to load config from {}", config_path))?;

    let (tracer_provider, meter_provider) = setup(&config)
        .inspect_err(|err| {
            println!("Failed to setup OTEL: {}", err);
        })
        .ok()
        .unzip();

    let meter = global::meter("pullix");

    debug!("Pullix starting...");

    let git = Git::new()?;

    match &config.home_manager {
        Some(hm_config) => {
            let nix_cmd = nix_commands::HomeManagerSwitch::new(hm_config);
            let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::HomeManager);
            let remote_state = RemoteStateMetric::new(&meter, DeploymentType::HomeManager);
            let service_handler = SystemdUserServiceHandler;
            let webhooks = WebhooksImpl::new(&config.webhooks)?;
            run_pullix(
                &config,
                &git,
                &nix_cmd,
                &nix_cmd,
                &service_handler,
                last_commit_metric,
                remote_state,
                &webhooks,
            )
            .await
            .inspect_err(|e| eprint!("{e}"))?
        }
        None => {
            let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
            let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);
            let service_handler = SystemdServiceHandler;
            let webhooks = WebhooksImpl::new(&config.webhooks)?;
            run_pullix(
                &config,
                &git,
                &nix_commands::Test,
                &nix_commands::Prod,
                &service_handler,
                last_commit_metric,
                remote_state,
                &webhooks,
            )
            .await
            .inspect_err(|e| eprint!("{e}"))?
        }
    };

    meter_provider.inspect(|provider| {
        let _ = provider.shutdown();
    });
    tracer_provider.inspect(|provider| {
        let _ = provider.shutdown();
    });
    debug!("Pullix run completed successfully.");

    Ok(())
}
