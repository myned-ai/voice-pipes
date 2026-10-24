# How Voice Pipes compares

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

Honest, verifiable differences against the frameworks people usually
evaluate alongside. Rows are limited to what can be checked in the projects'
own documentation as of mid-2026; there are no performance claims here
because they depend on hardware and providers.

| Capability | **Voice Pipes** | Pipecat | LiveKit Agents | Vocode |
|---|---|---|---|---|
| Language | **Rust** | Python | Python, Node | Python |
| License | Apache-2.0 | BSD-2 | Apache-2.0 | MIT |
| Fully local STT + LLM + TTS + VAD, no cloud keys | **Yes** | TTS only | No | Whisper.cpp only |
| Barge-in / interruption | Yes, with word-exact truncation | Yes | Yes | Partial |
| Semantic turn detection | Smart Turn, layered with provider signals | Smart Turn | Transformer model | Punctuation |
| Per-turn, per-component latency audit trail | **Built in** | No | No | No |
| A/B pipeline benchmarking and synthetic caller | **Built in** | No | Testing and simulations | No |
| Orchestrator with worker registry and dispatch | **Built in** | Single process; Pipecat Cloud | LiveKit server (SFU) | No |
| gRPC split mode (any stage in another process) | **Yes** | No | No | No |
| Telephony (PSTN/SIP) | Not yet ([roadmap](../roadmap.md)) | Daily, Twilio, Telnyx, Plivo, Exotel | Built-in SIP | Twilio |
| Client SDKs | WebSocket, documented protocol; TypeScript core in the admin console | JS, React, React Native, iOS, Android, C++ | Extensive | React |
| Provider ecosystem | ~20 built in, plugin SDK | 100+ | Large | Moderate |
| OpenTelemetry | Yes | Plugin | Partial | No |
| Transport | WebSocket; WebRTC gated stubs | WebRTC via Daily/LiveKit, WebSocket | WebRTC | Telephony WebSocket |

## Where the others are ahead

- **Telephony.** Every peer has a phone path; Voice Pipes does not yet. Since
  the transport already speaks WebSocket PCM, a Twilio or Telnyx media-streams
  adapter is bounded work and is first on the roadmap.
- **Client SDK breadth.** Pipecat and LiveKit ship packaged SDKs for six or
  more platforms. Voice Pipes documents the protocol for any language and
  ships a TypeScript core that is not yet its own package.
- **Provider count.** Pipecat's integration library is several times larger.
  Voice Pipes covers the providers that matter for a local-first stack and
  the major cloud ones, and the plugin SDK keeps the rest out of tree.
- **Community size.** They are older and bigger.

## Where Voice Pipes is ahead

- **Fully offline.** VAD, STT, LLM and TTS all run locally; no per-minute API
  cost and no audio leaves the machine. Air-gapped and edge deployments work.
- **Observability as a product feature.** Every turn records per-component
  latency, the audio of both sides, the events between them and what the
  client actually played. When a reply feels slow you can see which stage
  spent the milliseconds and whether the client's buffer, not the model, is
  to blame.
- **Benchmarking and evals in the box.** Two pipeline variants can be run
  side by side against the same scenarios with a synthetic caller and
  deterministic scoring, and the results diff as JSON.
- **Native concurrency.** Every stage runs on native threads with channel
  hand-off; there is no interpreter lock to block the audio path while a
  model runs. A worker is a single binary.
- **Fleet management without a media server.** Registration, heartbeats,
  least-connections dispatch, drain and aggregated history come with the
  orchestrator.

## What this table does not say

Which framework feels best in a real conversation depends on the providers
you pick and the network between them far more than on the framework. Use
the [benchmarking guide](../guides/benchmarking-and-evals.md) to measure your
own stack instead of trusting anyone's headline number.
