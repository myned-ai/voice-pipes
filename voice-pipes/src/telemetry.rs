//! Observability setup -- tracing subscriber and OpenTelemetry export.

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

/// Attempt to build an OTLP exporter. Returns None if it can't be configured.
#[cfg(feature = "telemetry")]
fn try_init_otel() -> Option<TracerProvider> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .ok()?;

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
