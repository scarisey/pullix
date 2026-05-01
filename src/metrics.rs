use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{WithExportConfig, SpanExporter};
use opentelemetry_sdk::{Resource, metrics::SdkMeterProvider, trace::SdkTracerProvider};

use crate::{config::Config, deploy::Deployed, git::Commit};

pub struct RemoteStateMetric(opentelemetry::metrics::Gauge<i64>);
impl RemoteStateMetric {
    pub fn new(meter: &opentelemetry::metrics::Meter) -> Self {
        let gauge = meter
            .i64_gauge("pullix_remote_state")
            .with_description("Get the remote state of the git repository.")
            .build();
        RemoteStateMetric(gauge)
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
        ];
        self.0.record(1, &labels);
    }
}

pub struct LastCommitMetric(opentelemetry::metrics::Gauge<i64>);

impl LastCommitMetric {
    pub fn new(meter: &opentelemetry::metrics::Meter) -> Self {
        let gauge = meter
            .i64_gauge("pullix_last_deployment")
            .with_description("Get the last commit deployed to the host.")
            .build();
        LastCommitMetric(gauge)
    }
    pub fn set(&self, commit: &Deployed) {
        let labels = match commit {
            Deployed::Init => return,
            Deployed::TestAligned(commit) => vec![
                KeyValue::new("deployed", "test"),
                KeyValue::new("commit", commit.to_string()),
            ],
            Deployed::ProdAligned(commit) => vec![
                KeyValue::new("deployed", "prod"),
                KeyValue::new("commit", commit.to_string()),
            ],
            Deployed::TestFailed(commit) => vec![
                KeyValue::new("deployed", "fail"),
                KeyValue::new("commit", commit.to_string()),
            ],
            Deployed::ProdFailed(commit) => vec![
                KeyValue::new("deployed", "fail"),
                KeyValue::new("commit", commit.to_string()),
            ],
        };
        self.0.record(1, &labels);
    }
}

pub fn setup_otel(config: &Config) -> Option<SdkMeterProvider> {
    config.otel_http_endpoint.as_ref().and_then(|endpoint| {
        let resource = Resource::builder().with_service_name("pullix").build();

        // Set up trace exporter
        let span_exporter = SpanExporter::builder()
            .with_http()
            .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
            .with_endpoint(endpoint)
            .build()
            .ok()?;
        let tracer_provider = SdkTracerProvider::builder()
            .with_simple_exporter(span_exporter)
            .with_resource(resource.clone())
            .build();
        global::set_tracer_provider(tracer_provider);

        // Initialize OTLP exporter using HTTP binary protocol
        let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
            .with_endpoint(endpoint)
            .with_temporality(opentelemetry_sdk::metrics::Temporality::Delta)
            .build()
            .ok()?;
        // Create a meter provider with the OTLP Metric exporter
        let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource)
            .build();
        global::set_meter_provider(meter_provider.clone());
        Some(meter_provider)
    })
}
