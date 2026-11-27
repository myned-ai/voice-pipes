//! Observability setup -- tracing subscriber and OpenTelemetry export.

pub mod resources;

#[cfg(feature = "telemetry")]
use opentelemetry::trace::TracerProvider as _;
#[cfg(feature = "telemetry")]
use opentelemetry_sdk::trace::TracerProvider;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

/// Initialize the telemetry stack:
/// 1. Local logging via tracing-subscriber (compact dev / JSON prod via `RUST_LOG`),
///    on stderr: stdout carries only a command's own output, which tools parse
/// 2. OpenTelemetry tracing via OTLP exporter (sends to `OTel` Collector if available)
///
/// When the `telemetry` feature is disabled, only local logging is configured
/// and the return value is always `()`.
#[cfg(feature = "telemetry")]
pub fn init(json_logs: bool) -> Option<TracerProvider> {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("voice_pipes=info"));

    // Try to set up OTel OTLP exporter -- gracefully skip if collector not available
    let otel_provider = try_init_otel();

    let registry = tracing_subscriber::registry().with(env_filter);

    if json_logs {
        let json_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .json();
        if let Some(ref provider) = otel_provider {
            let tracer = provider.tracer("voice-pipes");
            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
            registry.with(json_layer).with(otel_layer).init();
        } else {
            registry.with(json_layer).init();
        }
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .compact();
        if let Some(ref provider) = otel_provider {
            let tracer = provider.tracer("voice-pipes");
            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
            registry.with(fmt_layer).with(otel_layer).init();
        } else {
            registry.with(fmt_layer).init();
        }
    }

    otel_provider
}

#[cfg(not(feature = "telemetry"))]
/// Initialise the tracing subscriber (no-op OpenTelemetry variant).
pub fn init(json_logs: bool) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("voice_pipes=info"));

    let registry = tracing_subscriber::registry().with(env_filter);

    if json_logs {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .json(),
            )
            .init();
    } else {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .compact(),
            )
            .init();
    }
}

/// Validate that an OTLP endpoint uses TLS (`https://`) for any non-localhost
/// destination so API keys (`OTEL_EXPORTER_OTLP_HEADERS`) never travel over
/// plaintext HTTP to remote collectors.
pub fn is_allowed_otlp_endpoint(endpoint: &str) -> bool {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return true;
    }
    if trimmed.starts_with("https://") {
        return true;
    }
    if let Some(rest) = trimmed.strip_prefix("http://") {
        let host = rest.split('/').next().unwrap_or("").trim_start_matches('[');
        let host_no_port = if host.starts_with("::1]") {
            "::1"
        } else {
            host.split(':').next().unwrap_or("")
        };
        return matches!(host_no_port, "localhost" | "127.0.0.1" | "::1");
    }
    false
}

/// Attempt to build an OTLP exporter. Returns None if it can't be configured
/// or if a remote endpoint violates the TLS policy.
///
/// Honors standard OpenTelemetry environment variables:
/// - `OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc` default, or `http/protobuf`)
/// - `OTEL_EXPORTER_OTLP_ENDPOINT` / `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`
/// - `OTEL_EXPORTER_OTLP_HEADERS` (e.g. `X-API-Key=...`)
#[cfg(feature = "telemetry")]
fn try_init_otel() -> Option<TracerProvider> {
    if let Ok(ep) = std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .or_else(|_| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT"))
        && !is_allowed_otlp_endpoint(&ep)
    {
        eprintln!("voice-pipes: refusing plaintext non-localhost OTLP endpoint (use https://)");
        return None;
    }

    let protocol =
        std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").unwrap_or_else(|_| "grpc".to_string());

    let exporter = if protocol.eq_ignore_ascii_case("http/protobuf")
        || protocol.eq_ignore_ascii_case("http")
    {
        opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .build()
            .ok()?
    } else {
        opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .build()
            .ok()?
    };

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .build();

    Some(provider)
}

/// Flush and shut down the `OTel` pipeline so no spans are lost.
#[cfg(feature = "telemetry")]
pub fn shutdown(provider: Option<TracerProvider>) {
    if let Some(provider) = provider
        && let Err(e) = provider.shutdown()
    {
        // Expected when no OTel collector is running -- don't alarm the user.
        tracing::debug!("OTel shutdown (collector not reachable): {e}");
    }
}

#[cfg(not(feature = "telemetry"))]
/// Shut down telemetry (no-op when the `telemetry` feature is off).
pub fn shutdown(_provider: ()) {}

#[cfg(test)]
mod tests {
    use super::is_allowed_otlp_endpoint;

    #[test]
    fn otlp_endpoint_enforces_tls_for_remote_hosts() {
        assert!(is_allowed_otlp_endpoint(
            "https://otel.example.com/v1/traces"
        ));
        assert!(is_allowed_otlp_endpoint("http://localhost:4318/v1/traces"));
        assert!(is_allowed_otlp_endpoint("http://127.0.0.1:4317"));
        assert!(is_allowed_otlp_endpoint("http://[::1]:4318"));
        assert!(!is_allowed_otlp_endpoint(
            "http://otel.example.com/v1/traces"
        ));
        assert!(!is_allowed_otlp_endpoint("http://10.0.0.5:4318"));
        assert!(!is_allowed_otlp_endpoint("ftp://example.com"));
    }
}
