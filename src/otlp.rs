use std::time::Duration;

use opentelemetry::{trace::TracerProvider, KeyValue};
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use opentelemetry_semantic_conventions::{
    attribute::{DEPLOYMENT_ENVIRONMENT_NAME, SERVICE_NAME, SERVICE_VERSION},
    SCHEMA_URL,
};
use reqwest::Url;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer;

const OTEL_ENDPOINT: &str = "OTEL_ENDPOINT";
const OTEL_PROTOCOL: &str = "OTEL_PROTOCOL";
const OTEL_LEVEL: &str = "OTEL_LEVEL";
const OTEL_TIMEOUT: &str = "OTEL_TIMEOUT";
const OTEL_ENVIRONMENT: &str = "OTEL_ENVIRONMENT_NAME";

pub struct OtelGuard(SdkTracerProvider, tracing::Level);

impl OtelGuard {
    /// Get a tracer from the provider.
    fn tracer(&self, s: &'static str) -> opentelemetry_sdk::trace::Tracer {
        self.0.tracer(s)
    }

    /// Create a filtered tracing layer.
    pub fn layer<S>(&self) -> impl Layer<S>
    where
        S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    {
        let tracer = self.tracer("tracing-otel-subscriber");
        tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_filter(LevelFilter::from_level(self.1))
    }
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        if let Err(err) = self.0.shutdown() {
            eprintln!("{err:?}");
        }
    }
}

/// OTLP protocol options.
#[derive(Debug, Clone, Copy)]
pub enum OtlpProtocols {
    /// GRPC.
    Grpc,
    /// Binary.
    Binary,
    /// JSON.
    Json,
}

impl std::str::FromStr for OtlpProtocols {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            s if s.eq_ignore_ascii_case("grpc") => Ok(Self::Grpc),
            s if s.eq_ignore_ascii_case("binary") => Ok(Self::Binary),
            s if s.eq_ignore_ascii_case("json") => Ok(Self::Json),
            _ => Err(format!("Invalid protocol: {}", s)),
        }
    }
}

impl From<OtlpProtocols> for opentelemetry_otlp::Protocol {
    fn from(protocol: OtlpProtocols) -> Self {
        match protocol {
            OtlpProtocols::Grpc => Self::Grpc,
            OtlpProtocols::Binary => Self::HttpBinary,
            OtlpProtocols::Json => Self::HttpJson,
        }
    }
}

/// Otel configuration
#[derive(Debug, Clone)]
pub struct OtelConfig {
    /// The endpoint to send traces to, should be some valid HTTP endpoint for
    /// OTLP.
    pub endpoint: Url,
    /// Defaults to JSON.
    pub protocol: OtlpProtocols,
    /// Defaults to DEBUG.
    pub level: tracing::Level,
    /// Defaults to 1 second. Specified in Milliseconds.
    pub timeout: Duration,

    /// OTEL convenition `deployment.environment.name`
    pub environment: String,
}

impl OtelConfig {
    /// Load from env vars.
    pub fn load() -> Option<Self> {
        let endpoint = std::env::var(OTEL_ENDPOINT).ok()?.parse().ok()?;

        let protocol = std::env::var(OTEL_PROTOCOL)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(OtlpProtocols::Json);

        let level = std::env::var(OTEL_LEVEL)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(tracing::Level::DEBUG);

        let timeout = Duration::from_millis(
            std::env::var(OTEL_TIMEOUT).ok().and_then(|v| v.parse().ok()).unwrap_or(1000),
        );

        let environment =
            std::env::var(OTEL_ENVIRONMENT).ok().unwrap_or_else(|| "unknown".to_owned());

        Some(Self { endpoint, protocol, level, timeout, environment })
    }

    fn resource(&self) -> Resource {
        Resource::builder()
            .with_schema_url(
                [
                    KeyValue::new(SERVICE_NAME, env!("CARGO_PKG_NAME")),
                    KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
                    KeyValue::new(DEPLOYMENT_ENVIRONMENT_NAME, self.environment.clone()),
                ],
                SCHEMA_URL,
            )
            .build()
    }

    pub fn provider(&self) -> OtelGuard {
        let exporter = opentelemetry_otlp::SpanExporter::builder().with_http().build().unwrap();

        let provider = SdkTracerProvider::builder()
            // Customize sampling strategy
            // If export trace to AWS X-Ray, you can use XrayIdGenerator
            .with_resource(self.resource())
            .with_batch_exporter(exporter)
            .build();

        OtelGuard(provider, self.level)
    }
}
