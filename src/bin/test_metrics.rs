use anyhow::Result;
use log::info;
use metrics::{counter, gauge};
use std::time::Duration;

fn main() -> Result<()> {
    env_logger::init();

    // Set environment variables for OTLP collector:
    //
    // - OTEL_EXPORTER_OTLP_METRICS_ENDPOINT
    // - OTEL_EXPORTER_OTLP_METRICS_HEADERS (key1=value1,key2=value2)
    // - OTEL_METRIC_EXPORT_INTERVAL (milliseconds)
    //
    // On the HyperDX UI, always narrow metrics by their hostname
    // and service name attributes, e.g. in Lucene:
    //
    // ScopeAttributes.host.name:"localhost"
    // ScopeAttributes.service.name:"test_metrics"
    {
        use metrics_exporter_opentelemetry::Recorder;
        use opentelemetry::KeyValue;
        use opentelemetry_otlp::MetricExporterBuilder;
        let otlp_exporter = MetricExporterBuilder::new()
            .with_http()
            .build()
            .expect("failed to build otlp exporter");
        let _recorder = Recorder::builder(env!("CARGO_PKG_NAME"))
            .with_instrumentation_scope(|scope| {
                scope.with_attributes([
                    KeyValue::new("host.name", "localhost"),
                    KeyValue::new("service.name", "test_metrics"),
                ])
            })
            .with_meter_provider(|mpb| {
                // Periodically push out with our OTLP exporter
                mpb.with_periodic_exporter(otlp_exporter)
            })
            .install_global()
            .unwrap();
    }

    let gauge = gauge!("test_metrics.foo");
    let counter = counter!("test_metrics.bar");

    for _ in 0..20 {
        info!("setting gauge and counter");
        gauge.set(2.0);
        counter.increment(1);
        std::thread::sleep(Duration::from_secs(1));
    }

    std::thread::sleep(Duration::from_secs(10));

    Ok(())
}
