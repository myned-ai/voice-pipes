# Voice Pipes — Feature Example Pipelines

This directory (`voice-pipes/examples/features/`) together with sibling folders under `voice-pipes/examples/` provides ready-to-run pipeline configurations for every major Voice Pipes capability.

---

## Feature Matrix & Pipeline Compatibility

| Feature | Pipeline Mode | Cargo Feature(s) | Example Config |
| :--- | :--- | :--- | :--- |
| **VAP Turn Detection** (`mode = "vap"`, stereo floor prediction + lead-time `expected_end_in`) | Cascade (`STT -> LLM -> TTS`), `serve` & `live` | `turn-vap` | [`examples/features/turn-vap.toml`](turn-vap.toml) |
| **Smart Turn Detection** (`mode = "smart"`, Pipecat Whisper 8 s acoustic endpointer) | Cascade (`STT -> LLM -> TTS`), `serve` & `live` | `turn-smart` | [`examples/test-live-parakeet-smart.toml`](../test-live-parakeet-smart.toml) |
| **Turn-Detection Corpus Recording & Offline Scoring** (`record_session_audio = true`) | Cascade (`serve` & `live`) + `voice-pipes-bench` | `turn-smart`, `turn-vap` | [`examples/live/english-turn-corpus.toml`](../live/english-turn-corpus.toml) |
| **Speculative Execution** (`[speculative]` partial LLM prefill + speculative TTS) | Cascade (`STT -> LLM -> TTS`), `serve` | *(built-in)* | [`examples/features/speculative-execution.toml`](speculative-execution.toml) |
| **Adaptive Network Quality & Custom Grading** (`[adaptive_quality]`, `[grading]`) | Cascade & Realtime S2S, `serve` | *(built-in)* | [`examples/features/adaptive-quality.toml`](adaptive-quality.toml) |
| **External gRPC Agent (`AgentService`) + Idle Prompts** (`agent-grpc`, `[agent]`) | Cascade (as brain) & S2S (as tool backend), `serve` | *(built-in)* | [`examples/features/agent-grpc-idle.toml`](agent-grpc-idle.toml) |
| **Realtime S2S + Uplink Endpoint Observer** (`[realtime_provider.endpoint_observer]`) | Realtime S2S (`openai-realtime`, `gemini-live`, `elevenlabs`), `serve` | `realtime` | [`examples/features/realtime-s2s-endpoint-observer.toml`](realtime-s2s-endpoint-observer.toml) |
| **Avatar Annotators & Client-Side Tools** (`[[audio_post_processor]] mode = "annotate"`, `[[client_tool]]`) | Cascade & Realtime S2S, `serve` | *(built-in)* / `realtime` | [`examples/avatar/avatar-cascade-clienttools.toml`](../avatar/avatar-cascade-clienttools.toml), [`examples/avatar/avatar-realtime-clienttools.toml`](../avatar/avatar-realtime-clienttools.toml) |
| **Distributed gRPC Stage Offloading** (`remote-grpc` TTS/STT worker split) | Cascade, `serve` | *(built-in)* | [`examples/grpc/pipeline-remote-tts.toml`](../grpc/pipeline-remote-tts.toml), [`examples/grpc/worker-tts.toml`](../grpc/worker-tts.toml) |
| **Fleet Orchestration** (Orchestrator + multi-worker registration) | All modes (`orchestrate` + `serve`) | *(built-in)* | [`examples/orchestrate/orchestrator.toml`](../orchestrate/orchestrator.toml), [`examples/orchestrate/worker.toml`](../orchestrate/worker.toml) |
| **TTS Pronunciation Lexicon & Markdown Sanitizer** (`[tts.lexicon]`, `sanitize_markdown`, `passthrough_tags`) | Cascade (`STT -> LLM -> TTS`), `live` & `serve` | *(built-in)* | [`examples/features/tts-lexicon-and-tags.toml`](tts-lexicon-and-tags.toml) |

---

## Quick-Start Commands

Run commands from the `voice-pipes/` crate directory (or pass `voice-pipes/examples/...` from the workspace root):

### 1. VAP Two-Channel Turn Detection (`turn-vap.toml`)
```sh
# 1. Export the 2-second fixed-window VAP ONNX graph once:
python scripts/export-vap-onnx.py \
  --vap-root VAP --window-seconds 2 --out voice-pipes/models/vap/vap-2s.onnx

# 2. Run as a WebSocket worker (Playground / client SDKs):
cd voice-pipes
cargo run --release -p voice-pipes-cli --features turn-vap -- \
  --config examples/features/turn-vap.toml serve

# Or run with local microphone + speaker:
cargo run --release -p voice-pipes-cli --features turn-vap -- \
  --config examples/features/turn-vap.toml live --save-input captures
```

### 2. Pipecat Smart Turn Detection (`test-live-parakeet-smart.toml`)
```sh
cd voice-pipes
cargo run --release -p voice-pipes-cli --features turn-smart -- \
  --config examples/test-live-parakeet-smart.toml live
```

### 3. Speculative Execution (`speculative-execution.toml`)
```sh
cd voice-pipes
cargo run --release -p voice-pipes-cli -- \
  --config examples/features/speculative-execution.toml serve
```

### 4. Adaptive Network Quality & Custom SLA Grading (`adaptive-quality.toml`)
```sh
cd voice-pipes
cargo run --release -p voice-pipes-cli -- \
  --config examples/features/adaptive-quality.toml serve
```

### 5. External gRPC `AgentService` + Idle Re-engagement (`agent-grpc-idle.toml`)
```sh
# Terminal 1: run the reference Python gRPC agent
cd examples/reference-agent
python reference_agent.py --port 50061

# Terminal 2: run the worker connected to the gRPC agent
cd voice-pipes
cargo run --release -p voice-pipes-cli -- \
  --config examples/features/agent-grpc-idle.toml serve
```

### 6. Realtime S2S with Endpoint Observer (`realtime-s2s-endpoint-observer.toml`)
```sh
cd voice-pipes
cargo run --release -p voice-pipes-cli --features realtime -- \
  --config examples/features/realtime-s2s-endpoint-observer.toml serve
```

### 7. Validate Any Example Config in CI
```sh
cd voice-pipes
cargo run -p voice-pipes-cli -- validate-config examples/features/speculative-execution.toml
```
