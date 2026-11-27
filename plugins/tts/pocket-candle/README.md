# `voice-pipes-tts-pocket-candle`

Native pure-Rust (**Candle**) implementation of **Kyutai Pocket-TTS** (`FlowLM` + LSD flow matching + streaming `Mimi` decoder & encoder) for Voice Pipes.

## Key Highlights
- **Zero-copy shared voice KV prefixes**: Immutable `Arc<VoiceKvPrefix>` shared across all concurrent users via online split-attention (no per-request KV cloning).
- **Pipelined FlowLM ↔ Mimi streaming**: Emits native 80 ms (1920-sample @ 24 kHz) frames while overlapping latent generation and waveform decoding.
- **Zero-shot voice cloning**: Supports both precomputed `.safetensors` voice states and on-the-fly `.wav` voice cloning through the built-in Mimi encoder (cached in a bounded LRU).
- **Dual-mode deployment**:
  - **In-process plugin** (`provider = "pocket-candle"`) via `--features tts-pocket-candle`.
  - **Standalone HTTP server** (`pocket-tts-server`) via `--features server`, speaking the exact `POST /tts` streamed WAV contract for `pocket-http` horizontal scaling.

## Quick Build & Test
```sh
cargo test -p voice-pipes-tts-pocket-candle
cargo build --release -p voice-pipes-tts-pocket-candle --features server
```
