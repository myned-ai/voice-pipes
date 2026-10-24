# Design decisions

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

Short records of decisions that shaped the engine and that a contributor
would otherwise re-litigate. Each says what the situation was, what was
decided, what it costs, and where the code is. They are distilled from the
implementation plans that drove the work; the plans themselves are internal.

| Decision | One line |
|---|---|
| [Component trait and health registry](component-trait.md) | Every stage is a `Component` with counters, a latency window and a health state; health is passive and observed, not polled |
| [Open-schema per-turn metrics](per-turn-metrics-registry.md) | Metrics are a list of component spans with descriptors, so a new component is one new module, not a nine-file edit |
| [Pipeline error propagation and session modes](pipeline-error-propagation.md) | One `message` envelope, typed error severity and component, and five session modes negotiated at handshake |
| [Provider factory registry](provider-registry.md) | Providers register a factory; configuration never names a concrete type |
| [Client-side tools](client-side-tools.md) | The worker never executes a tool; the LLM's call is routed to the client and the result fenced to its turn |
| [Off-path audio annotators](audio-annotators.md) | Metadata derived from audio runs teed, batched and asynchronous so it can never slow the audio down |
| [Throughput observability](throughput-observability.md) | Real-time factors on the output stage and per-processor budgets, because a stream that trickles is invisible to latency metrics |
| [Honest speech-to-speech latency](s2s-latency-measurement.md) | Time to first byte is anchored on the first audio, with the anchor's provenance on the wire and the provider's endpointing lag measured separately |
| [STT session lifecycle](stt-session-lifecycle.md) | Parse every server frame, classify capacity apart from auth, and terminate or deadline every session on every exit path |

## Writing one

Copy the template below into `explanation/decisions/<slug>.md`, keep it
under sixty lines, and add a row above.

```markdown
# <Decision>

**Status:** active
**Last verified:** YYYY-MM-DD
**Owner:** Docs
**Decided:** YYYY-MM

## Context
## Decision
## Consequences
## Where in code
```
