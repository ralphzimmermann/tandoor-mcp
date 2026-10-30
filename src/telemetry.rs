//! Optional OpenTelemetry trace export to New Relic.
//!
//! A non-empty `NEW_RELIC_LICENSE_KEY` is the feature switch. Without it the
//! process installs only the existing formatted-log subscriber and creates no
//! exporter, background task, or outbound telemetry traffic.

use std::{borrow::Cow, collections::HashMap, env};

use anyhow::{bail, Context, Result};
use opentelemetry::{global, trace::TracerProvider as _, KeyValue};
use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    runtime,
    trace::{span_processor_with_async_runtime::BatchSpanProcessor, Sampler, SdkTracerProvider},
    Resource,
};
use sentry::integrations::tracing::{BreadcrumbLayer, ErrorLayer, SpanLayer};
use tracing_subscriber::{
    filter::LevelFilter, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer,
};

const DEFAULT_APP_NAME: &str = "tandoor-mcp";
const DEFAULT_OTLP_ENDPOINT: &str = "https://otlp.nr-data.net:4318";

/// Optional Sentry settings loaded before the Tokio runtime starts.
#[derive(Debug, PartialEq)]
pub struct SentryConfig {
    dsn: Option<String>,
    environment: Option<String>,
    release: Option<String>,
    traces_sample_rate: f32,
}

impl SentryConfig {
    pub fn from_env() -> Result<Self> {
        Self::from_values(
            env::var("SENTRY_DSN").ok(),
            env::var("SENTRY_ENVIRONMENT").ok(),
            env::var("SENTRY_RELEASE").ok(),
            env::var("SENTRY_TRACES_SAMPLE_RATE").ok(),
        )
    }

    fn from_values(
        dsn: Option<String>,
        environment: Option<String>,
        release: Option<String>,
        traces_sample_rate: Option<String>,
    ) -> Result<Self> {
        let traces_sample_rate = non_empty(traces_sample_rate)
            .map(|value| {
                value
                    .parse::<f32>()
                    .context("SENTRY_TRACES_SAMPLE_RATE must be a number")
            })
            .transpose()?
            .unwrap_or(1.0);
        if !(0.0..=1.0).contains(&traces_sample_rate) {
            bail!("SENTRY_TRACES_SAMPLE_RATE must be between 0 and 1");
        }

        Ok(Self {
            dsn: non_empty(dsn),
            environment: non_empty(environment),
            release: non_empty(release),
            traces_sample_rate,
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.dsn.is_some()
    }
}

/// Initialize Sentry before Tokio starts so worker threads inherit the hub.
///
/// An absent DSN still returns a guard backed by a disabled client. Keeping one
/// path avoids initializing the tracing subscriber differently by environment.
pub fn init_sentry(config: SentryConfig) -> Result<sentry::ClientInitGuard> {
    let release = config
        .release
        .map(Cow::Owned)
        .or_else(|| sentry::release_name!());
    let mut options = sentry::ClientOptions::new()
        .maybe_release(release)
        .traces_sample_rate(config.traces_sample_rate)
        .send_default_pii(false);

    if let Some(dsn) = config.dsn {
        dsn.parse::<sentry::types::Dsn>()
            .context("SENTRY_DSN is invalid")?;
        options = options.dsn(&dsn);
    }
    if let Some(environment) = config.environment {
        options = options.environment(environment);
    }

    Ok(sentry::init(options))
}

/// New Relic settings loaded from the process environment.
pub struct TelemetryConfig {
    license_key: String,
    app_name: String,
    endpoint: String,
    /// Optional `deployment.environment.name` resource attribute. Left unset
    /// rather than defaulted so a developer machine with a key never reports
    /// itself as production.
    environment: Option<String>,
}

impl TelemetryConfig {
    /// Return `None` when the license key is absent or blank.
    pub fn from_env() -> Option<Self> {
        Self::from_values(
            env::var("NEW_RELIC_LICENSE_KEY").ok(),
            env::var("NEW_RELIC_APP_NAME").ok(),
            env::var("NEW_RELIC_OTLP_ENDPOINT").ok(),
            env::var("NEW_RELIC_ENVIRONMENT_NAME").ok(),
        )
    }

    fn from_values(
        license_key: Option<String>,
        app_name: Option<String>,
        endpoint: Option<String>,
        environment: Option<String>,
    ) -> Option<Self> {
        let license_key = non_empty(license_key)?;
        let endpoint = non_empty(endpoint).unwrap_or_else(|| DEFAULT_OTLP_ENDPOINT.to_owned());
        Some(Self {
            license_key,
            app_name: non_empty(app_name).unwrap_or_else(|| DEFAULT_APP_NAME.to_owned()),
            endpoint: trace_endpoint(&endpoint),
            environment: non_empty(environment),
        })
    }
}

/// Install formatted logging and, when configured, an OTLP trace exporter.
///
/// The returned provider must remain alive for the process lifetime and be
/// shut down during graceful termination to flush buffered spans.
pub fn init(config: Option<TelemetryConfig>) -> Result<Option<SdkTracerProvider>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".to_string().into());

    let Some(config) = config else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .with(SpanLayer::new().with_filter(LevelFilter::INFO))
            .with(ErrorLayer::new().with_filter(LevelFilter::ERROR))
            .with(BreadcrumbLayer::new().with_filter(LevelFilter::INFO))
            .init();
        return Ok(None);
    };

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(config.endpoint)
        .with_headers(HashMap::from([("api-key".to_owned(), config.license_key)]))
        .build()
        .context("could not build the New Relic OTLP exporter")?;

    // `Resource::builder()` keeps the SDK detector so `telemetry.sdk.*`
    // attributes reach New Relic, which uses them for entity language and
    // instrumentation-version views.
    let mut attributes = vec![KeyValue::new("service.version", env!("CARGO_PKG_VERSION"))];
    if let Some(environment) = config.environment {
        attributes.push(KeyValue::new("deployment.environment.name", environment));
    }
    let resource = Resource::builder()
        .with_service_name(config.app_name)
        .with_attributes(attributes)
        .build();
    // The HTTP exporter is async and must run on Tokio. The SDK's default
    // thread-based batch processor has no Tokio reactor and panics on export.
    let batch_processor = BatchSpanProcessor::builder(exporter, runtime::Tokio).build();
    let provider = SdkTracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .with_resource(resource)
        .with_span_processor(batch_processor)
        .build();

    global::set_text_map_propagator(TraceContextPropagator::new());
    let tracer = provider.tracer(env!("CARGO_PKG_NAME"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .with(SpanLayer::new().with_filter(LevelFilter::INFO))
        .with(ErrorLayer::new().with_filter(LevelFilter::ERROR))
        .with(BreadcrumbLayer::new().with_filter(LevelFilter::INFO))
        .init();

    Ok(Some(provider))
}

/// Flush buffered spans and stop the exporter.
///
/// The Tokio-runtime batch processor's `shutdown` blocks the calling thread on
/// a oneshot that its own Tokio task must answer (it ignores its timeout). From
/// the root future that is merely a stalled main thread; from a spawned task
/// on a single-worker runtime (one-vCPU hosts) it deadlocks the whole runtime.
/// Running it on the blocking pool is safe from either place.
pub async fn shutdown(provider: SdkTracerProvider) {
    let result = tokio::task::spawn_blocking(move || provider.shutdown()).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "Could not flush all telemetry during shutdown")
        }
        Err(error) => tracing::warn!(%error, "Telemetry shutdown task failed"),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// `with_endpoint` expects the complete signal URL; unlike the standard OTLP
/// environment variable, it does not append `/v1/traces` to a base URL.
fn trace_endpoint(endpoint: &str) -> String {
    let endpoint = endpoint.trim_end_matches('/');
    if endpoint.ends_with("/v1/traces") {
        endpoint.to_owned()
    } else {
        format!("{endpoint}/v1/traces")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        trace_endpoint, SentryConfig, TelemetryConfig, DEFAULT_APP_NAME, DEFAULT_OTLP_ENDPOINT,
    };

    #[test]
    fn blank_sentry_dsn_disables_sentry() {
        let config = SentryConfig::from_values(Some("  ".to_owned()), None, None, None).unwrap();
        assert!(!config.is_enabled());
        assert_eq!(config.traces_sample_rate, 1.0);
    }

    #[test]
    fn sentry_settings_are_trimmed_and_parsed() {
        let config = SentryConfig::from_values(
            Some(" https://key@example.test/1 ".to_owned()),
            Some(" staging ".to_owned()),
            Some(" tandoor-mcp@abc ".to_owned()),
            Some(" 0.25 ".to_owned()),
        )
        .unwrap();
        assert!(config.is_enabled());
        assert_eq!(config.environment.as_deref(), Some("staging"));
        assert_eq!(config.release.as_deref(), Some("tandoor-mcp@abc"));
        assert_eq!(config.traces_sample_rate, 0.25);
    }

    #[test]
    fn sentry_sample_rate_must_be_valid() {
        assert!(SentryConfig::from_values(None, None, None, Some("abc".to_owned())).is_err());
        assert!(SentryConfig::from_values(None, None, None, Some("1.1".to_owned())).is_err());
    }

    #[test]
    fn absent_or_blank_key_disables_telemetry() {
        assert!(TelemetryConfig::from_values(None, None, None, None).is_none());
        assert!(TelemetryConfig::from_values(Some(" \n\t".to_owned()), None, None, None).is_none());
    }

    #[test]
    fn defaults_are_applied() {
        let config =
            TelemetryConfig::from_values(Some("license".to_owned()), None, None, None).unwrap();
        assert_eq!(config.app_name, DEFAULT_APP_NAME);
        assert_eq!(
            config.endpoint,
            format!("{DEFAULT_OTLP_ENDPOINT}/v1/traces")
        );
        assert_eq!(config.environment, None);
    }

    #[test]
    fn configured_values_are_trimmed() {
        let config = TelemetryConfig::from_values(
            Some(" license ".to_owned()),
            Some(" recipes-mcp ".to_owned()),
            Some(" https://example.test:4318 ".to_owned()),
            Some(" staging ".to_owned()),
        )
        .unwrap();
        assert_eq!(config.license_key, "license");
        assert_eq!(config.app_name, "recipes-mcp");
        assert_eq!(config.endpoint, "https://example.test:4318/v1/traces");
        assert_eq!(config.environment.as_deref(), Some("staging"));
    }

    #[test]
    fn blank_environment_is_omitted() {
        let config = TelemetryConfig::from_values(
            Some("license".to_owned()),
            None,
            None,
            Some("   ".to_owned()),
        )
        .unwrap();
        assert_eq!(config.environment, None);
    }

    #[test]
    fn trace_endpoint_appends_signal_path_once() {
        assert_eq!(
            trace_endpoint("https://example.test:4318/"),
            "https://example.test:4318/v1/traces"
        );
        assert_eq!(
            trace_endpoint("https://example.test:4318/v1/traces"),
            "https://example.test:4318/v1/traces"
        );
    }
}
