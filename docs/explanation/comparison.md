# Architectural comparison

**Status:** active
**Last verified:** 2026-11-02
**Owner:** Docs

Teams building real-time voice agents generally choose between three architectural approaches:
1. **Compiled Pipeline Runtime (Voice Pipes)** — a single-binary Rust engine with built-in local ONNX/GGUF inference, cloud provider plugins, and engine-seam per-turn telemetry.
2. **Python / Node Voice Agent Frameworks** — higher-level orchestration libraries in Python or TypeScript/Node that wire cloud or local model bindings into an event loop or WebRTC SFU.
3. **Hosted Speech-to-Speech APIs** — end-to-end multimodal cloud APIs (such as OpenAI Realtime, Gemini Live, or ElevenLabs Agents) where a single vendor model handles audio in and audio out. *(Voice Pipes can also wrap these providers behind the same session protocol.)*

There are no latency benchmarks on this page because real-world conversational latency depends primarily on the models you choose, the hardware they run on, and the network path between client and worker.

| Capability | **Voice Pipes** | Python / Node Agent Frameworks | Hosted Speech-to-Speech APIs |
|---|---|---|---|
| **Core runtime** | **Safe Rust (`#![forbid(unsafe_code)]`)**, single native binary | Python (`asyncio`) or Node.js runtime | Managed cloud service |
| **License** | **BUSL-1.1** (free production use under \$5M USD annual revenue; converts to Apache-2.0 after 3 years) | Open source (varies: MIT, BSD, Apache-2.0) | Proprietary per-minute API |
| **100% offline STT + LLM + TTS + VAD** | **Built in** (ONNX Runtime + GGUF in-process, no cloud keys required) | Possible via external model servers or Python bindings | No (cloud-only) |
| **Barge-in & history truncation** | **Built in**, playback-accurate truncation via client `PlaybackReport` (`last_played_offset_ms`) | Supported (truncation accuracy depends on transport and TTS alignment) | Provider-managed |
| **Semantic turn detection** | **Smart Turn (ONNX)** + Silero VAD + provider endpoint signals | Available in major frameworks | Provider-managed |
| **Per-turn, per-component latency audit trail** | **Built in** at engine trait seams ([VPO-Metrics](../reference/vpo-metrics.md) / [VPO-Telemetry](../reference/vpo-telemetry.md)) | Typically via external OpenTelemetry tracing or third-party observability plugins | Limited to provider-exposed events |
| **A/B pipeline benchmarking & synthetic caller** | **Built in** (`voice-pipes-bench` CLI & Admin Console) | External evaluation harnesses | External evaluation harnesses |
| **In-memory RL & regression gym** | **Built in** ([`voice-pipes-gym`](voice-pipes-gym.md), `<30 µs` deterministic resets) | External | N/A |
| **gRPC split mode (any stage out-of-process)** | **Built in** ([gRPC split mode](../extending/grpc-split-mode.md)) | Custom microservice wiring | N/A |
| **Orchestrator with worker registry & dispatch** | **Built in** (least-connections dispatch, drain, session rollups) | Via WebRTC SFU cluster or cloud platform | Vendor-managed |
| **Telephony (PSTN / SIP)** | Planned ([roadmap](../roadmap.md)) | Widely supported (Twilio, Telnyx, Daily, built-in SIP) | Via telephony bridges |
| **Packaged client SDKs** | Open WebSocket protocol ([VPO-Protocol](../reference/vpo-protocol.md)); TypeScript & React guides | Broad pre-packaged WebRTC SDKs (Web, iOS, Android, React Native) | Vendor WebSocket / WebRTC SDKs |

## Where other ecosystems have broader coverage today

- **Telephony (PSTN / SIP).** Established Python and WebRTC frameworks ship turnkey SIP and PSTN integrations (Twilio, Telnyx, Daily, LiveKit SIP). Voice Pipes does not yet ship a built-in telephony adapter; because its transport already operates over framed WebSocket PCM, a media-streams bridge is bounded work and is tracked on the [roadmap](../roadmap.md).
- **Pre-packaged mobile SDKs.** Mature WebRTC platforms provide packaged iOS, Android, Flutter, and React Native SDKs with built-in acoustic echo cancellation (AEC). Voice Pipes specifies a language-agnostic [WebSocket wire protocol](../clients/protocol.md) and ships TypeScript/React client modules in the Admin Console, leaving native mobile audio capture to the host application.
- **Long-tail SaaS integrations.** Python frameworks benefit from large community catalogs of 50–100+ SaaS wrappers. Voice Pipes focuses on ~20 high-signal local and cloud providers in-tree and exposes a SemVer-stable [`voice-pipes-sdk`](../extending/plugin-sdk.md) (`1.0`) for custom integrations.

## Where Voice Pipes is purpose-built

- **Fully offline, zero-key operation.** VAD, STT, turn detection, LLM, and TTS all run in-process on local CPU or CUDA via ONNX Runtime and GGUF. No audio leaves the machine and there is no per-minute API cost, making air-gapped, on-prem, and edge deployments straightforward.
- **Engine-seam observability as a core invariant.** Every turn records engine-measured stage spans (`llm_wait_ms`, `tts_wait_ms`, `output_wait_ms`), provider breakdowns, both-side WAV captures, client playback reports, and automated anomaly/verdict grading ([Metrics glossary](../reference/metrics-glossary.md)). When a turn feels slow, the session inspector shows whether the milliseconds were spent in endpointing, model inference, inline post-processing, or client network jitter.
- **Deterministic A/B benchmarking and RL simulation in the box.** Two pipeline TOML files can be evaluated side by side against the same audio scenarios with a synthetic caller and diffed as JSON, and [`voice-pipes-gym`](voice-pipes-gym.md) runs thousands of stateful multi-turn episodes per second for regression gating or GRPO/DPO training.
- **Predictable native concurrency.** Audio framing, VAD gating, and stage hand-offs run on native OS threads and bounded async channels without a garbage collector or interpreter lock on the real-time audio path, and a worker deploys as a single compiled binary.

## Measure your own stack

Which runtime feels best in production depends on the providers you configure and the network path to your users. Use the [benchmarking and evaluations guide](../guides/benchmarking-and-evals.md) to measure your own pipeline end-to-end with real audio scenarios rather than relying on generic latency claims.
