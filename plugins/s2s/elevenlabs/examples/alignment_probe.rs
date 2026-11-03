//! Wire probe for the `ElevenLabs` word-alignment streaming protocol.
//!
//! Connects to an `ElevenLabs` agent, sends one typed user message, and
//! dumps every inbound event's shape — specifically the `alignment`
//! payload on `audio` events — to answer:
//!   Q1: are `char_start_times_ms` chunk-relative or response-cumulative?
//!   Q2: does every audio chunk carry alignment (and/or `normalized_alignment`)?
//!   Q3: do the joined chars equal the `agent_response` text?
//!
//! Run:
//!   $`env:ELEVENLABS_API_KEY` = "..."
//!   cargo run -p voice-pipes-s2s-elevenlabs --example `alignment_probe` -- <`agent_id`>

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures_util::{SinkExt, StreamExt};
use voice_pipes_net::tokio_tungstenite::tungstenite::Message;
use voice_pipes_net::tokio_tungstenite::tungstenite::handshake::client::generate_key;
use voice_pipes_net::tokio_tungstenite::tungstenite::http::Request;

#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "throwaway wire probe; one linear dump loop is easier to read than helpers"
)]
async fn main() -> anyhow::Result<()> {
    voice_pipes_net::install_crypto_provider();
    let agent_id = std::env::args()
        .nth(1)
        .expect("usage: alignment_probe <agent_id>");
    let api_key = std::env::var("ELEVENLABS_API_KEY").expect("ELEVENLABS_API_KEY not set");

    let url = format!("wss://api.elevenlabs.io/v1/convai/conversation?agent_id={agent_id}");
    let req = Request::builder()
        .method("GET")
        .uri(&url)
        .header("Host", "api.elevenlabs.io")
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .header("xi-api-key", voice_pipes_net::sensitive_header(&api_key)?)
        .body(())?;

    // Frames carry base64 audio; bound them like the provider does.
    let (ws, _resp) = voice_pipes_net::connect_websocket(
        req,
        16 * 1024 * 1024,
        std::time::Duration::from_secs(10),
    )
    .await?;
    let (sink, mut stream) = ws.split();
    let sink = std::sync::Arc::new(tokio::sync::Mutex::new(sink));
    println!("connected");

    sink.lock()
        .await
        .send(Message::Text(
            serde_json::json!({ "type": "conversation_initiation_client_data" })
                .to_string()
                .into(),
        ))
        .await?;

    // Voice mode: second arg = 16 kHz mono s16le WAV to stream as mic
    // audio (100 ms chunks at realtime, then silence); no arg = typed
    // probe message.
    let wav_path = std::env::args().nth(2);
    if let Some(path) = &wav_path {
        let raw = std::fs::read(path)?;
        let pcm = raw[44..].to_vec(); // naive WAV header skip (16k mono s16le)
        let chunks: Vec<String> = pcm.chunks(3200).map(|c| BASE64.encode(c)).collect();
        println!("streaming {} ({} chunks of 100ms)", path, chunks.len());
        let sink_up = std::sync::Arc::clone(&sink);
        tokio::spawn(async move {
            for c in chunks {
                let msg = serde_json::json!({ "user_audio_chunk": c }).to_string();
                if sink_up
                    .lock()
                    .await
                    .send(Message::Text(msg.into()))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            // Keep uplinking silence so server VAD sees end-of-speech
            // and stays armed while the agent replies.
            let silence = BASE64.encode(vec![0u8; 3200]);
            for _ in 0..200 {
                let msg = serde_json::json!({ "user_audio_chunk": silence.clone() }).to_string();
                if sink_up
                    .lock()
                    .await
                    .send(Message::Text(msg.into()))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        });
    } else {
        sink.lock()
            .await
            .send(Message::Text(
                serde_json::json!({
                    "type": "user_message",
                    "text": "Please greet me with exactly two short sentences."
                })
                .to_string()
                .into(),
            ))
            .await?;
    }

    let mut audio_events = 0u32;
    let mut cumulative_audio_ms = 0f64;
    let started = std::time::Instant::now();
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(if wav_path.is_some() { 110 } else { 25 });

    loop {
        let msg = tokio::select! {
            m = stream.next() => match m { Some(Ok(m)) => m, _ => break },
            () = tokio::time::sleep_until(deadline) => break,
        };
        let Message::Text(text) = msg else { continue };
        let v: serde_json::Value = serde_json::from_str(&text)?;
        let ty = v["type"].as_str().unwrap_or("?");
        match ty {
            "ping" => {
                let id = v["ping_event"]["event_id"].clone();
                sink.lock()
                    .await
                    .send(Message::Text(
                        serde_json::json!({ "type": "pong", "event_id": id })
                            .to_string()
                            .into(),
                    ))
                    .await?;
            }
            "audio" => {
                audio_events += 1;
                let ev = &v["audio_event"];
                let pcm_bytes = ev["audio_base_64"]
                    .as_str()
                    .map_or(0, |b| BASE64.decode(b).map_or(0, |d| d.len()));
                // Assume pcm_16000 (agent default) for duration math.
                let chunk_ms = (pcm_bytes as f64 / 2.0) / 16.0;
                let alignment = &ev["alignment"];
                let has_alignment = alignment.is_object();
                let keys: Vec<&str> = ev
                    .as_object()
                    .map(|o| o.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                println!(
                    "[{:6}ms] audio #{audio_events}: event_id={} is_final={} chunk_ms={chunk_ms:.0} cumulative_before_ms={cumulative_audio_ms:.0} keys={keys:?} alignment={}",
                    started.elapsed().as_millis(),
                    ev["event_id"],
                    ev["is_final"],
                    if has_alignment { "yes" } else { "NO" },
                );
                if has_alignment {
                    let chars: String = alignment["chars"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|c| c.as_str()).collect())
                        .unwrap_or_default();
                    let starts = alignment["char_start_times_ms"].as_array();
                    let (first, last) = starts.map_or((-1.0, -1.0), |a| {
                        (
                            a.first()
                                .and_then(serde_json::Value::as_f64)
                                .unwrap_or(-1.0),
                            a.last().and_then(serde_json::Value::as_f64).unwrap_or(-1.0),
                        )
                    });
                    println!(
                        "  chars({})=\"{}\" first_start_ms={first:.0} last_start_ms={last:.0}",
                        chars.chars().count(),
                        chars.replace('\n', "\\n"),
                    );
                    if audio_events <= 2 {
                        println!("  RAW alignment: {alignment}");
                    }
                }
                cumulative_audio_ms += chunk_ms;
            }
            "agent_response" => {
                println!(
                    "[{:6}ms] agent_response: \"{}\"",
                    started.elapsed().as_millis(),
                    v["agent_response_event"]["agent_response"]
                        .as_str()
                        .unwrap_or("")
                );
            }
            "conversation_initiation_metadata" => {
                println!(
                    "metadata: out={} in={}",
                    v["conversation_initiation_metadata_event"]["agent_output_audio_format"],
                    v["conversation_initiation_metadata_event"]["user_input_audio_format"],
                );
            }
            other => {
                let mut s = v.to_string();
                s.truncate(300);
                println!("[{:6}ms] event: {other} {s}", started.elapsed().as_millis());
            }
        }
    }
    println!("done: {audio_events} audio events, ~{cumulative_audio_ms:.0} ms total audio");
    Ok(())
}
