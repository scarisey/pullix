use anyhow::{Context, Result};
use opentelemetry::{global, metrics::MeterProvider};
use pullix::{
    config::Config,
    git::Git,
    metrics::{LastCommitMetric, RemoteStateMetric, setup_otel},
    *,
};
use tracing::{Level, debug, span};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let root = span!(Level::TRACE, "pullix_start");
    let _ = root.enter();
    debug!("Pullix starting...");

    let config_path = std::env::var("PULLIX_CONFIG").context("Can't find PULLIX_CONFIG env var")?;
    let config = Config::load_from_path(&config_path)
        .with_context(|| format!("Failed to load config from {}", config_path))?;
    let git = Git::new();

    let meter_provider = setup_otel(&config);
    let meter = meter_provider
        .as_ref()
        .map(|meter_provider| meter_provider.meter("pullix"))
        .unwrap_or(global::meter("pullix"));

    let last_commit_metric = LastCommitMetric::new(&meter);
    let remote_state = RemoteStateMetric::new(&meter);

    match &config.home_manager_command {
        Some(hm_cmd) => {
            let nix_cmd = nix_commands::HomeManagerSwitch::new(hm_cmd.clone());
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
        None => run_pullix(
            &config,
            &git,
            &nix_commands::Test,
            &nix_commands::Prod,
            last_commit_metric,
            remote_state,
        )
        .await
        .inspect_err(|e| eprint!("{e}"))?,
    };

    meter_provider.inspect(|provider| {
        let _ = provider.shutdown();
    });
    debug!("Pullix run completed successfully.");

    Ok(())
}
