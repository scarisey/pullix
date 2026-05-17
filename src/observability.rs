use std::time::Duration;

use derive_more::Display;
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::{
    Resource,
    metrics::{PeriodicReader, SdkMeterProvider},
    trace::SdkTracerProvider,
};
use tracing_subscriber::{Registry, layer::SubscriberExt};

use crate::{config::Config, deploy::Deployed, git::Commit};
use anyhow::Result;
use opentelemetry::trace::TracerProvider as _;

#[derive(Display)]
pub enum DeploymentType {
    HomeManager,
    NixOS,
}

pub struct RemoteStateMetric {
    gauge: opentelemetry::metrics::Gauge<i64>,
    deployment_type: DeploymentType,
}
impl RemoteStateMetric {
    pub fn new(meter: &opentelemetry::metrics::Meter, deployment_type: DeploymentType) -> Self {
        let gauge = meter
            .i64_gauge("pullix_remote_state")
            .with_description("Get the remote state of the git repository.")
            .build();
        RemoteStateMetric {
            gauge,
            deployment_type,
        }
    }
    pub fn set(
        &self,
        main_commit: &Commit,
        prod_commit: Option<&Commit>,
        test_commit: Option<&Commit>,
    ) {
        let labels = vec![
            KeyValue::new("main_commit", main_commit.to_string()),
            prod_commit
                .map(|c| KeyValue::new("prod_commit", c.to_string()))
                .unwrap_or(KeyValue::new("prod_commit", "unknown")),
            test_commit
                .map(|c| KeyValue::new("test_commit", c.to_string()))
                .unwrap_or(KeyValue::new("test_commit", "unknown")),
            KeyValue::new("deployment_type", self.deployment_type.to_string()),
        ];
        self.gauge.record(1, &labels);
    }
}

pub struct LastCommitMetric {
    gauge: opentelemetry::metrics::Gauge<i64>,
    deployment_type: DeploymentType,
}

impl LastCommitMetric {
    pub fn new(meter: &opentelemetry::metrics::Meter, deployment_type: DeploymentType) -> Self {
        let gauge = meter
            .i64_gauge("pullix_last_deployment")
            .with_description("Get the last commit deployed to the host.")
            .build();
        LastCommitMetric {
            gauge,
            deployment_type,
        }
    }
    pub fn set(&self, commit: &Deployed) {
        let labels = match commit {
            Deployed::Init => return,
            Deployed::TestAligned(commit) => vec![
                KeyValue::new("deployed", "test"),
                KeyValue::new("commit", commit.to_string()),
                KeyValue::new("deployment_type", self.deployment_type.to_string()),
            ],
            Deployed::ProdAligned(commit) => vec![
                KeyValue::new("deployed", "prod"),
                KeyValue::new("commit", commit.to_string()),
                KeyValue::new("deployment_type", self.deployment_type.to_string()),
            ],
            Deployed::TestFailed(commit) => vec![
                KeyValue::new("deployed", "fail"),
                KeyValue::new("commit", commit.to_string()),
                KeyValue::new("deployment_type", self.deployment_type.to_string()),
            ],
            Deployed::ProdFailed(commit) => vec![
                KeyValue::new("deployed", "fail"),
                KeyValue::new("commit", commit.to_string()),
                KeyValue::new("deployment_type", self.deployment_type.to_string()),
            ],
        };
        self.gauge.record(1, &labels);
    }
}

pub fn setup(config: &Config) -> Result<(SdkTracerProvider, SdkMeterProvider)> {
    let endpoint = config
        .otel_http_endpoint
        .as_ref()
        .ok_or(anyhow::anyhow!("Otel endpoint not provided."))?;
    let resource = Resource::builder().with_service_name("pullix").build();

    // Set up trace exporter
    let span_exporter = SpanExporter::builder()
        .with_http()
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .with_endpoint(format!("{}/v1/traces", endpoint))
        .build()?;
    let tracer_provider = SdkTracerProvider::builder()
        .with_simple_exporter(span_exporter)
        .with_resource(resource.clone())
        .build();

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));

    let otel_traces_layer = config.otel_http_endpoint.as_ref().map(|_| {
        let tracer = tracer_provider.tracer("pullix");
        tracing_opentelemetry::layer().with_tracer(tracer)
    });
    let tracer_subscriber = Registry::default()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(true)
                .with_file(true)
                .with_line_number(true),
        )
        .with(otel_traces_layer);
    tracing::subscriber::set_global_default(tracer_subscriber).expect("Unable to set tracer");

    // Initialize OTLP exporter using HTTP binary protocol
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .with_endpoint(format!("{}/v1/metrics", endpoint))
        .with_temporality(opentelemetry_sdk::metrics::Temporality::Delta)
        .build()?;
    let periodic_reader = PeriodicReader::builder(metric_exporter)
        .with_interval(Duration::from_secs(config.poll_interval_secs))
        .build();
    // Create a meter provider with the OTLP Metric exporter
    let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_reader(periodic_reader)
        .with_resource(resource)
        .build();
    global::set_meter_provider(meter_provider.clone());

    Ok((tracer_provider, meter_provider))
}
