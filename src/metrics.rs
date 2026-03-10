use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{Resource, metrics::SdkMeterProvider};

use crate::{config::Config, deploy::Deployed};

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
        // Initialize OTLP exporter using HTTP binary protocol
        let exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
            .with_endpoint(endpoint)
            .with_temporality(opentelemetry_sdk::metrics::Temporality::Delta)
            .build()
            .ok()?;
        // Create a meter provider with the OTLP Metric exporter
        let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_periodic_exporter(exporter)
            .with_resource(Resource::builder().with_service_name("pullix").build())
            .build();
        global::set_meter_provider(meter_provider.clone());
        Some(meter_provider)
    })
}
