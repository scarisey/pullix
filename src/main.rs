use anyhow::{Context, Result};
use opentelemetry::{
    global,
    metrics::MeterProvider,
    trace::{Tracer, TracerProvider as _},
};
use pullix::{
    config::Config,
    git::Git,
    metrics::{DeploymentType, LastCommitMetric, RemoteStateMetric, setup_otel},
    systemd::{SystemdServiceHandler, SystemdUserServiceHandler},
    *,
};
use tracing::{Level, debug, span};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::var("PULLIX_CONFIG").context("Can't find PULLIX_CONFIG env var")?;
    let config = Config::load_from_path(&config_path)
        .with_context(|| format!("Failed to load config from {}", config_path))?;

    let (tracer_provider, meter_provider) = setup_otel(&config)
        .inspect_err(|err| {
            println!("Failed to setup OTEL: {}", err);
        })
        .ok()
        .unzip();

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));

    let subscriber = tracing_subscriber::registry().with(env_filter).with(
        tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_thread_ids(true)
            .with_file(true)
            .with_line_number(true),
    );
    if config.otel_http_endpoint.is_some() {
        if let Some(ref provider) = tracer_provider {
            let tracer = provider.tracer("pullix");
            let tracer_telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
            subscriber.with(tracer_telemetry).init();
        }
    } else {
        subscriber.init();
    }

    let meter = meter_provider
        .as_ref()
        .map(|meter_provider| meter_provider.meter("pullix"))
        .unwrap_or(global::meter("pullix"));

    let root = span!(Level::INFO, "pullix_start");
    let _ = root.enter();
    debug!("Pullix starting...");

    let git = Git::new()?;

    match &config.home_manager {
        Some(hm_config) => {
            let nix_cmd = nix_commands::HomeManagerSwitch::new(hm_config);
            let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::HomeManager);
            let remote_state = RemoteStateMetric::new(&meter, DeploymentType::HomeManager);
            let service_handler = SystemdUserServiceHandler;
            run_pullix(
                &config,
                &git,
                &nix_cmd,
                &nix_cmd,
                &service_handler,
                last_commit_metric,
                remote_state,
            )
            .await
            .inspect_err(|e| eprint!("{e}"))?
        }
        None => {
            let last_commit_metric = LastCommitMetric::new(&meter, DeploymentType::NixOS);
            let remote_state = RemoteStateMetric::new(&meter, DeploymentType::NixOS);
            let service_handler = SystemdServiceHandler;
            run_pullix(
                &config,
                &git,
                &nix_commands::Test,
                &nix_commands::Prod,
                &service_handler,
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
    tracer_provider.inspect(|provider| {
        let _ = provider.shutdown();
    });
    debug!("Pullix run completed successfully.");

    Ok(())
}
