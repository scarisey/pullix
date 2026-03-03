use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::metrics::SdkMeterProvider;

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
    config
        .prometheus_exporter_endpoint
        .as_ref()
        .and_then(|endpoint| {
            // Initialize OTLP exporter using HTTP binary protocol
            let exporter = opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .build()
                .ok()?;
            // Create a meter provider with the OTLP Metric exporter
            let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
                .with_periodic_exporter(exporter)
                .build();
            global::set_meter_provider(meter_provider.clone());
            Some(meter_provider)
        })
}
