use anyhow::{Context, Result};
use opentelemetry::{global, metrics::MeterProvider};
use pullix::{
    config::Config,
    git::Git,
    metrics::{DeploymentType, LastCommitMetric, RemoteStateMetric, setup_otel},
    *,
};
use tracing::{Level, debug, span};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::var("PULLIX_CONFIG").context("Can't find PULLIX_CONFIG env var")?;
    let config = Config::load_from_path(&config_path)
        .with_context(|| format!("Failed to load config from {}", config_path))?;

    let meter_provider = setup_otel(&config);

    if config.otel_http_endpoint.is_some() {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_file(true)
                    .with_line_number(true),
            )
            .with(tracing_opentelemetry::layer())
            .init();
    } else {
        tracing_subscriber::fmt::init();
    }

    let root = span!(Level::TRACE, "pullix_start");
    let _ = root.enter();
    debug!("Pullix starting...");

    let git = Git::new()?;

    let meter = meter_provider
        .as_ref()
        .map(|meter_provider| meter_provider.meter("pullix"))
        .unwrap_or(global::meter("pullix"));

    match &config.home_manager {
        Some(hm_config) => {
            let nix_cmd = nix_commands::HomeManagerSwitch::new(hm_config);
            let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::HomeManager);
            let remote_state = RemoteStateMetric::new(&meter, DeploymentType::HomeManager);
            run_pullix(
                &config,
                &git,
                &nix_cmd,
                &nix_cmd,
                last_commit_metric,
                remote_state,
            )
            .await
            .inspect_err(|e| eprint!("{e}"))?
        }
        None => {
            let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
            let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);
            run_pullix(
                &config,
                &git,
                &nix_commands::Test,
                &nix_commands::Prod,
                last_commit_metric,
                remote_state,
            )
            .await
            .inspect_err(|e| eprint!("{e}"))?
        }
    };

    meter_provider.inspect(|provider| {
        let _ = provider.shutdown();
    });
    debug!("Pullix run completed successfully.");

    Ok(())
}
