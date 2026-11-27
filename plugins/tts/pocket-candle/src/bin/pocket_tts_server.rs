//! Standalone horizontally scalable Rust HTTP server for Kyutai Pocket-TTS.
//!
//! Drop-in replacement for Python `pocket-tts serve` speaking the exact
//! `POST /tts` chunked WAV streaming protocol (`24 kHz mono i16 PCM`), with:
//! - True multi-threaded Rust/Candle synthesis sharing immutable weights (`Arc<PocketEngine>`)
//! - Zero-copy shared voice KV prefixes (`Arc<VoiceKvPrefix>`)
//! - Immediate cancellation on HTTP client disconnect (barge-in friendly)
//! - Bounded concurrency (`429 Too Many Requests` backpressure), body-size limits,
//!   optional constant-time Bearer token auth, `/health`, and `/metrics` (Prometheus).

#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation
)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Form, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use candle_core::Device;
use clap::Parser;
use serde::Deserialize;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use voice_pipes_sdk::AudioFrame;

use voice_pipes_tts_pocket_candle::server_support::{
    PocketCandleConfig, PocketEngine, SAMPLE_RATE, VoiceCache,
};

#[derive(Debug, Parser)]
#[command(
    name = "pocket-tts-server",
    about = "Native Rust/Candle Pocket-TTS HTTP server"
)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 8002)]
    port: u16,
    #[arg(
        long,
        env = "POCKET_TTS_MODEL_DIR",
        default_value = "models/pocket-tts"
    )]
    model_dir: PathBuf,
    #[arg(long, env = "POCKET_TTS_VOICES_DIR")]
    voices_dir: Option<PathBuf>,
    #[arg(long, env = "POCKET_TTS_VOICE", default_value = "alba")]
    voice: String,
    #[arg(long, env = "POCKET_TTS_MAX_CONCURRENT", default_value_t = 4)]
    max_concurrent: usize,
    #[arg(long, env = "POCKET_TTS_TOKEN")]
    bearer_token: Option<String>,
}

struct AppState {
    engine: Arc<PocketEngine>,
    voices: Arc<VoiceCache>,
    config: PocketCandleConfig,
    semaphore: Arc<Semaphore>,
    bearer_token: Option<String>,
    active_requests: AtomicUsize,
    total_requests: AtomicU64,
    rejected_requests: AtomicU64,
}

#[derive(Debug, Deserialize)]
struct TtsForm {
    text: String,
    #[serde(default)]
    voice_url: Option<String>,
}

/// RAII guard that cancels the synthesis token and decrements active count if the
/// HTTP client disconnects mid-stream (essential for low-latency voice barge-in).
struct StreamCancelGuard {
    cancel: CancellationToken,
    state: Arc<AppState>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for StreamCancelGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.state.active_requests.fetch_sub(1, Ordering::Relaxed);
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn check_auth(headers: &HeaderMap, expected: Option<&str>) -> bool {
    let Some(secret) = expected else {
        return true;
    };
    let Some(raw) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    constant_time_eq(raw.trim().as_bytes(), secret.as_bytes())
}

fn streaming_wav_header() -> Vec<u8> {
    let byte_rate = SAMPLE_RATE * 2;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16_u32.to_le_bytes());
    h.extend_from_slice(&1_u16.to_le_bytes());
    h.extend_from_slice(&1_u16.to_le_bytes());
    h.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&2_u16.to_le_bytes());
    h.extend_from_slice(&16_u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
    h
}

async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    axum::Json(serde_json::json!({
        "status": "ok",
        "active_requests": state.active_requests.load(Ordering::Relaxed),
        "max_concurrent": state.config.max_concurrent,
    }))
}

async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let active = state.active_requests.load(Ordering::Relaxed);
    let total = state.total_requests.load(Ordering::Relaxed);
    let rejected = state.rejected_requests.load(Ordering::Relaxed);
    let body = format!(
        "# HELP pocket_tts_active_requests Active concurrent TTS streams.\n\
         # TYPE pocket_tts_active_requests gauge\n\
         pocket_tts_active_requests {active}\n\
         # HELP pocket_tts_max_concurrent Maximum configured concurrent TTS streams.\n\
         # TYPE pocket_tts_max_concurrent gauge\n\
         pocket_tts_max_concurrent {}\n\
         # HELP pocket_tts_requests_total Total accepted TTS requests.\n\
         # TYPE pocket_tts_requests_total counter\n\
         pocket_tts_requests_total {total}\n\
         # HELP pocket_tts_rejected_total Total 429 capacity-rejected requests.\n\
         # TYPE pocket_tts_rejected_total counter\n\
         pocket_tts_rejected_total {rejected}\n",
        state.config.max_concurrent
    );
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

async fn tts_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<TtsForm>,
) -> Response {
    if !check_auth(&headers, state.bearer_token.as_deref()) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let Ok(permit) = Arc::clone(&state.semaphore).try_acquire_owned() else {
        state.rejected_requests.fetch_add(1, Ordering::Relaxed);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "1")],
            "server at max_concurrent capacity",
        )
            .into_response();
    };

    state.total_requests.fetch_add(1, Ordering::Relaxed);
    state.active_requests.fetch_add(1, Ordering::Relaxed);

    let voice_name = form
        .voice_url
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| (*state.config.voice.load_full()).clone());
    let (tx, mut rx) = mpsc::channel::<AudioFrame>(8);
    let cancel = CancellationToken::new();

    let engine = Arc::clone(&state.engine);
    let voices = Arc::clone(&state.voices);
    let cfg = state.config.clone();
    let text = form.text;
    let cancel_worker = cancel.clone();

    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        match voices.get_or_load(&engine, &cfg.voices_dir, &voice_name) {
            Ok(vstate) => {
                if let Err(err) =
                    engine.synthesize_blocking(&text, &vstate, &cfg, &tx, &cancel_worker)
                {
                    warn!(
                        error = %err,
                        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                        "synthesis error"
                    );
                }
            }
            Err(err) => {
                warn!(error = %err, "voice load error");
            }
        }
    });

    let guard = StreamCancelGuard {
        cancel,
        state: Arc::clone(&state),
        _permit: permit,
    };

    let body_stream = async_stream::stream! {
        let _guard = guard;
        yield Ok::<Vec<u8>, std::io::Error>(streaming_wav_header());
        while let Some(frame) = rx.recv().await {
            let mut bytes = Vec::with_capacity(frame.samples.len() * 2);
            for s in frame.samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            yield Ok::<Vec<u8>, std::io::Error>(bytes);
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/wav")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let device = {
        #[cfg(feature = "cuda")]
        {
            Device::new_cuda(0).unwrap_or(Device::Cpu)
        }
        #[cfg(not(feature = "cuda"))]
        {
            Device::Cpu
        }
    };

    let engine = Arc::new(PocketEngine::load_from_dir(&cli.model_dir, device)?);
    let voices_dir = cli
        .voices_dir
        .unwrap_or_else(|| cli.model_dir.join("voices"));
    let mut tts_cfg = voice_pipes_sdk::TtsConfig::new("pocket-candle").with_voice(Some(cli.voice));
    tts_cfg.options.insert(
        "model_dir".into(),
        serde_json::json!(cli.model_dir.display().to_string()),
    );
    tts_cfg.options.insert(
        "voices_dir".into(),
        serde_json::json!(voices_dir.display().to_string()),
    );
    tts_cfg.options.insert(
        "max_concurrent".into(),
        serde_json::json!(cli.max_concurrent.clamp(1, 64)),
    );
    let config = PocketCandleConfig::from_tts_config(&tts_cfg)?;
    let voices = Arc::new(VoiceCache::new(config.max_cached_voices));

    let state = Arc::new(AppState {
        engine,
        voices,
        semaphore: Arc::new(Semaphore::new(config.max_concurrent)),
        config,
        bearer_token: cli.bearer_token,
        active_requests: AtomicUsize::new(0),
        total_requests: AtomicU64::new(0),
        rejected_requests: AtomicU64::new(0),
    });

    let app = Router::new()
        .route("/tts", post(tts_handler))
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;
    info!(%addr, "pocket-tts-server listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
