# Gateway Core: Extension and Operations Guide

Last updated: 2026-09-19
Owner: Backend
Status: Active

This is the authoritative reference for **extending** the voice-pipes session
gateway (adding a client protocol or an inference engine) and for
**operating/debugging** it (the turn-state lifecycle and the control-plane
wire trace).

The canonical wire contract remains
[MESSAGES-AND-EVENTS-SPEC.md](../clients/protocol.md); this guide links to
it rather than restating message shapes.

---

## 1. The two extension surfaces

The gateway is deliberately built around two existing trait surfaces plus two
small internal seams. To extend the system you implement one trait; you do
**not** add a parallel orchestration path.

| You want to add… | You implement… | You do NOT touch… |
|---|---|---|
| A new client transport (WebTransport, an avatar protocol, …) | [`TransportSession`](../../voice-pipes/src/transport/session.rs), plus a route in [`serve/router.rs`](../../voice-pipes/src/transport/serve/router.rs) that accepts the connection and hands the session to `run_session` | The runners, the turn-state machine, the engine drivers |
| A new inference engine (a new realtime provider, a new classic stack) | The [`EngineDriver`](../../voice-pipes/src/transport/serve/gateway/engine_driver.rs) seam (+ the existing provider traits) | The transport layer, the shared control handlers |

Everything between the two surfaces — ping/timing handling, state transitions,
the control-message wire shapes — is shared and must stay shared.

---

## 2. Adding a new client protocol (`TransportSession`)

A client adapter translates a wire protocol into the canonical
`ClientMessage` / `ServerMessage` types at the edge. The core orchestration
only ever sees canonical types, so the transport choice is invisible to it.

### 2.1 Implement `TransportSession`

`TransportSession` is protocol-agnostic
([session.rs](../../voice-pipes/src/transport/session.rs)):

```rust
#[async_trait]
pub trait TransportSession: Send + Sync {
    async fn recv(&mut self) -> Result<Option<ClientMessage>, TransportError>;
    async fn send(&self, msg: ServerMessage) -> Result<(), TransportError>;
    async fn close(&self, reason: &str) -> Result<(), TransportError>;
    fn quality(&self) -> ConnectionQuality;
    fn metadata(&self) -> &SessionMetadata;
}
```

Reference implementations: `WsSession` (WebSocket) and `DirectSession`
(in-process mpsc, used by the CLI and tests). These are the only two that
exist. WebTransport is not implemented: the worker's `/wt` and
`/webtransport` routes answer with a structured `501 Not Implemented` that
points clients back to `/ws`, and the admin console's WebRTC and WebTransport
adapters are feature-gated stubs whose send methods throw. A WebTransport
`WtSession` is planned but not yet implemented.

Rules for a correct adapter:

1. **Translate at the edge.** If your protocol's frames don't match the
   canonical messages, map them inside `recv`/`send`. Never leak a
   protocol-specific shape into the core.
2. **`recv` returns `Ok(None)` on a clean close**, `Err(..)` on a protocol
   error or unexpected disconnect. The session manager relies on this
   distinction.
3. **Do not re-order or coalesce control frames.** The control-plane ordering
   is contractual (see §4) and is pinned by golden fixtures.

### 2.2 Accept the connection

There is no acceptor trait. Each transport owns its listen/upgrade step as a
route on the worker's axum router
([router.rs](../../voice-pipes/src/transport/serve/router.rs)): the `/ws`
handler upgrades the socket, wraps it in a `WsSession` with a
`ConnectionHealthMonitor`, and hands it to `run_session`, which performs the
`configure`/`resume` handshake and negotiation
([setup.rs](../../voice-pipes/src/transport/serve/setup.rs)). A new transport
adds its own route next to `/ws` and calls the same `run_session`; the
existing routes are not modified.

### 2.3 Conformance target

A new adapter is "done" when it passes the golden protocol traces (§4). The
classic happy-path trace lives at
[../../voice-pipes/src/transport/serve/gateway/tests/fixtures/classic_happy_path.trace](../../voice-pipes/src/transport/serve/gateway/tests/fixtures/classic_happy_path.trace);
drive your adapter through the same lifecycle and assert the same normalized
control trace.

---

## 3. Adding a new engine (`EngineDriver`)

Most of an engine's behavior is already shared. The only parts that
legitimately differ per engine are captured by the `EngineDriver` seam
([engine_driver.rs](../../voice-pipes/src/transport/serve/gateway/engine_driver.rs)):

```rust
pub(crate) trait EngineDriver {
    /// Abort the in-flight turn at the engine boundary.
    async fn cancel_turn(&mut self) -> anyhow::Result<()>;

    /// Recover the assistant text actually heard for `utterance_id`
    /// at the given playback `offset_ms`.
    fn resolve_played_text(&self, utterance_id: u32, offset_ms: u32) -> Option<String>;
}
```

How the two shipped engines implement it:

| Operation | Classic (`ClassicSessionContext`) | Realtime (`RealtimeSessionContext`) |
|---|---|---|
| `cancel_turn` | Fires the turn's `CancellationToken` | Sends a provider cancel envelope via `RealtimeClient::cancel` |
| `resolve_played_text` | Looks up the live `AudioLedger` (`text_at_offset`) | Binary-searches the frozen `text_timeline` of the `PendingInterruption` |

`resolve_played_text` semantics: return `None` when nothing can be resolved
(unknown utterance, no ledger/timeline, or a zero offset); an **empty string**
is a distinct, meaningful result meaning "the client played none of the
assistant audio." A client-supplied `last_played_text` always wins over the
seam result.

### 3.1 What is NOT in the seam (and why)

Turn-metrics finalization is **intentionally not** an `EngineDriver` method.
It is reached from five call sites with divergent surrounding orchestration,
and the realtime interrupt path must also emit a `PendingInterruption` that a
single `-> TurnMetricsSnapshot` signature would silently drop. Forcing it into
the trait would be a leaky abstraction. It stays per-runner until interrupt-path
golden fixtures exist (see the plan, Phase 3 step 5).

### 3.2 Drive transitions through the state machine

A new engine must **not** mutate `TurnState` directly. Route every transition
through [`state_machine::apply`](../../voice-pipes/src/transport/serve/gateway/state_machine.rs)
with the appropriate `TurnTrigger`. Adding a new `TurnState` or `TurnTrigger`
variant is a compile error until every transition is reconsidered — that is the
safety property; do not bypass it with raw `set_turn_state` calls.

### 3.3 Reuse the shared handlers

`ping` and client `timing_report` are handled once in
[shared_handlers.rs](../../voice-pipes/src/transport/serve/gateway/shared_handlers.rs).
A new engine calls these rather than re-implementing them.

---

## 4. Control-plane wire contract and golden fixtures

The gateway emits a small, ordered set of control frames per turn. That
ordering is contractual and is pinned by a Phase 0 golden regression net so
that later refactors cannot silently change it.

- **Normalizer:** [golden.rs](../../voice-pipes/src/transport/serve/gateway/golden.rs)
  maps each `ServerControl` frame to a stable token (e.g.
  `state_changed:speaking`, `turn_metrics:interrupted=false`,
  `clear_audio:{reason}`), dropping volatile fields (ids, timestamps,
  latencies) and collapsing consecutive duplicates.
- **Driver + assertion:** `golden_classic_happy_path_control_trace` in
  [../../voice-pipes/src/transport/serve/tests/suite/golden.rs](../../voice-pipes/src/transport/serve/tests/suite/golden.rs)
  drives a full classic turn and compares the trace to the committed fixture.

Canonical classic happy-path trace:

```
session_ready
state_changed:processing
state_changed:speaking
turn_metrics:interrupted=false
state_changed:draining
state_changed:listening
```

Regenerate the fixture **intentionally** (after reviewing the diff) with:

```
UPDATE_GOLDEN=1 cargo test --features realtime \
  transport::serve::tests::golden_classic_happy_path_control_trace
```

A failing golden test with no intended protocol change means a regression —
do not regenerate to make it pass.

---

## 5. Operator runbook: state-machine-aware debugging

### 5.1 The turn lifecycle

`TurnState` is the single lifecycle authority
([state_machine.rs](../../voice-pipes/src/transport/serve/gateway/state_machine.rs)).
A healthy voice turn walks:

```
Listening → Processing → Speaking → Draining → Listening
```

Triggers (named for *what happened*, not the target state):

| Trigger | Meaning | Legal from |
|---|---|---|
| `UtteranceCommitted` | STT-final committed; LLM/TTS begins | `Listening` (→ `Processing`); no-op in `Processing`; rejected in `Speaking`/`Draining` |
| `AudioOutputStarted` | First assistant audio chunk streamed | any non-`Speaking` (→ `Speaking`) |
| `Draining` | Turn teardown (cancel, barge-in, normal end) | any (→ `Draining`); no-op if already `Draining` |
| `PlaybackFinalized` | Client ack / early-barge short-circuit | `Speaking`/`Draining` (→ `Listening`); **rejected** from `Listening`/`Processing` |

`apply` returns `Some(new_state)` only on a real change (the caller emits
`state_changed`); `None` means an idempotent no-op or a **rejected** illegal
transition. A rejected transition is silently ignored by design — it is not an
error, it is the guard working.

### 5.2 Reading the live state

Every real transition emits a `state_changed` control frame to the client
(see the trace tokens in §4). To follow a session's lifecycle, watch the
ordered `state_changed:*` tokens — the same surface the golden trace captures
and the Playground state-timeline panel (T6) renders.

### 5.3 Symptom → likely cause

| Symptom | Likely cause | Where to look |
|---|---|---|
| Stuck in `Processing`, never reaches `Speaking` | No first audio chunk emitted (engine produced no output, or output suppressed) | Engine `cancel_turn`/output path; `suppress_audio` flag |
| Stuck in `Draining`, never returns to `Listening` | Client never sent `PlaybackComplete`/`PlaybackReport`, so `PlaybackFinalized` never fired | Client adapter; `handle_playback_complete` |
| `state_changed:listening` arrives while audio is still playing | `PlaybackFinalized` fired too early (early-barge short-circuit) | `transition_early_barge_to_listening` in [turn.rs](../../voice-pipes/src/transport/serve/turn.rs) |
| Barge-in leaves stale assistant text in history | `resolve_played_text` returned the wrong prefix, or `last_played_offset_ms` was stale | `EngineDriver::resolve_played_text`; the `stale_playback_reason` guard |
| Truncation chops mid-word (realtime) | `text_timeline` snapshot vs whitespace-snap boundary | `RealtimeTurn::truncate_text_at` |
| Golden test fails after an unrelated change | A refactor changed control-frame ordering | Diff the failing trace against the fixture (§4) |
| Client connects (reaches `session_ready`/`listening`) then silently resets to disconnected, no error frame | Often **not** the worker you think: another process is bound to the same port (e.g. a leftover Docker worker on IPv6 `::1` while the native worker is on IPv4 `0.0.0.0`; on Windows `localhost` resolves to `::1` first). The wrong server accepts, then tears the session down. | `curl http://127.0.0.1:<port>/health` and `curl http://localhost:<port>/health` and compare `aggregate_health` + `components` — they will differ if two servers are listening. Check `netstat`/`Get-NetTCPConnection` for the port. |
| First turn fails with a backend connection error (`*_SYNTHESIS_FAILED`, network refused) | A required sidecar (e.g. local TTS server) isn't running | Connect and read the event log: a `severity:"warning"` `*_WARMUP_FAILED` frame now fires at connect; `GET /health` shows that component `unhealthy`. |

### 5.4 Invariants to assert when debugging

1. Every `state_changed` corresponds to a real transition (no duplicate
   consecutive states on the wire).
2. `PlaybackFinalized` is only honored from `Speaking`/`Draining`.
3. A client-supplied `last_played_text` overrides the seam's resolution.
4. No raw `set_turn_state` outside the state machine.
5. No wire-schema change without a reviewed golden-fixture update.

---

## 6. Related documents

- [MESSAGES-AND-EVENTS-SPEC.md](../clients/protocol.md) — canonical wire contract.
