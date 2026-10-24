# Voice Pipes documentation

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

Voice Pipes is a modular, observable real-time voice AI runtime in Rust. Plug
any STT, LLM and TTS provider into a streaming pipeline with neural VAD,
barge-in and per-turn metrics, or put a speech-to-speech provider behind the
same session protocol. The local stack runs with no cloud keys.

Pick the door that matches what you are doing.

| I want to… | Start here | Then |
|---|---|---|
| **Build a voice agent** — run the stack, pick providers, tune turn-taking, deploy | [Quickstart (Docker)](getting-started/quickstart-docker.md) · [Quickstart (source)](getting-started/quickstart-source.md) | [Your first agent](getting-started/first-agent.md) → [Guides](#guides) |
| **Connect a client** — a browser, an app, a device, over WebSocket | [Building a client](clients/building-a-client.md) | [Wire protocol](clients/protocol.md) · [Playground extensions](clients/playground-extensions.md) |
| **Write a provider or component** — a new STT/TTS/LLM, a post-processor, a transport | [Plugin SDK](extending/plugin-sdk.md) | [Custom component](extending/custom-component.md) · [Wizard descriptor](extending/wizard-descriptor.md) |

New to the vocabulary? Read [Concepts](getting-started/concepts.md) first: it
defines pipeline, session, turn, barge-in, provider and worker in one page.

## Getting started

- [Quickstart with Docker](getting-started/quickstart-docker.md) — one command, no host dependencies
- [Quickstart from source](getting-started/quickstart-source.md) — build the CLI, download models, run `live`
- [Your first agent](getting-started/first-agent.md) — change the LLM, the voice and the prompt; serve it; talk to it
- [Concepts](getting-started/concepts.md)

## Guides

- [Configuration](guides/configuration.md) — the TOML file, env vars, overrides, validation
- [Providers](guides/providers/README.md) — support matrix and one page per STT, TTS, LLM and speech-to-speech provider
- [Turn detection and interruptions](guides/turn-detection-and-interruptions.md)
- [Voice activity detection](guides/vad.md)
- [Tool calling](guides/tool-calling.md) — server-side and client-side tools
- [Audio annotators](guides/audio-annotators.md) — lip-sync, visemes and other per-audio metadata off the critical path
- [Observability](guides/observability.md) — per-turn metrics, the audit trail, OpenTelemetry
- [Benchmarking and evals](guides/benchmarking-and-evals.md) — A/B pipelines, the synthetic caller, regression suites
- [Fleet: orchestrator and workers](guides/fleet.md)
- [Deployment](guides/deployment.md) — the compose stack, sidecars, hardening
- [GPU and CUDA](guides/gpu-and-cuda.md)
- [Admin console](guides/admin-console.md) — playground, sessions, benchmarks
- [Troubleshooting](guides/troubleshooting.md)

## Clients

- [Building a client](clients/building-a-client.md) — the integration reference, any language
- [Wire protocol](clients/protocol.md) — every message, event and capture artefact
- [Protocol walkthrough](clients/protocol-walkthrough.md) — the same contract read as a session timeline
- [TypeScript SDK](clients/typescript-sdk.md) · [React SDK integration](clients/react-sdk-integration.md)
- [Playground extensions](clients/playground-extensions.md)

## Extending

- [Plugin SDK](extending/plugin-sdk.md) — STT, TTS and LLM providers in an external crate
- [Custom component](extending/custom-component.md) — anything with health and per-turn metrics
- [Wizard descriptor](extending/wizard-descriptor.md) — make a provider selectable in the setup wizard
- [gRPC split mode](extending/grpc-split-mode.md) — run any stage as a separate process
- [Gateway: transports and engines](extending/gateway-transports-and-engines.md)
- [Community integrations](extending/community-integrations.md)

## Reference

- [CLI](reference/cli.md) · [Configuration](reference/config.md) · [Cargo features](reference/cargo-features.md) · [Providers (wizard tables)](reference/providers.md)
- [Pipeline events](reference/events.md) · [HTTP API](reference/http-api.md) · [Admin API](reference/admin-api.md)
- [Metrics glossary](reference/metrics-glossary.md) · [VPO metrics](reference/vpo-metrics.md) · [VPO telemetry](reference/vpo-telemetry.md) · [VPO protocol](reference/vpo-protocol.md)
- [TTS benchmarking](reference/tts-benchmarking.md)

## Explanation

- [Streaming architecture](explanation/architecture.md)
- [The interruption model](explanation/interruption-model.md) · [Composable VAD](explanation/composable-vad.md) · [Per-turn metrics](explanation/per-turn-metrics.md)
- [LLM conversation history](explanation/llm-conversation-history.md) · [Session history](explanation/session-history.md) · [Admin console architecture](explanation/admin-console-architecture.md)
- [TTS streaming latency](explanation/tts-streaming-latency.md) · [STT provider comparison](explanation/stt-provider-comparison.md)
- [How Voice Pipes compares](explanation/comparison.md)
- [Design decisions](explanation/decisions/README.md)

## Contributing

- [Development setup](contributing/development-setup.md) · [Testing](contributing/testing.md) · [Dependencies](contributing/dependencies.md) · [Documentation style](contributing/docs-style.md) · [Release process](contributing/release-process.md)
- [Roadmap](roadmap.md)

Machine-readable index: [llms.txt](llms.txt).
