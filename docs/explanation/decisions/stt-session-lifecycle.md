# STT session lifecycle

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs
**Decided:** 2026-07

## Context

An AssemblyAI account hit its streaming concurrency limit. The server sent a
structured error and closed with a policy code; the client had no variant
for the error frame, classified the close as an authentication failure
(permanent), gave up the backoff loop, and was respawned on the next audio
chunk with zero delay, creating a reconnect loop that itself consumed the
remaining sessions. An inventory found six ways a server-side session could
be left open or churned, including per-utterance sessions whose terminate
handshake overlapped the next connect.

## Decision

Principles that apply to any streaming provider:

- **Parse everything the server says.** Every documented frame has a
  variant; classification is semantic (capacity, auth, transient) and done
  in one helper from the richest signal available.
- **Terminate or deadline on every exit path.** End of utterance, cancel,
  mid-session error and pre-connect wait all either send the provider's
  terminate message or arm a deadline; the cancel token is real and
  honoured.
- **Back off on respawn.** A dead worker task is not recreated at line
  speed.
- **Persistent sessions are opt-in.** `persistent_session = true` keeps one
  connection across utterances and sends a force-endpoint instead of
  terminating; off by default until proven in live runs.

Google STT, implemented later, follows the same principles with per-utterance
streams by default and proactive rotation before the provider's cap.

## Consequences

- Capacity errors are reported as capacity and retried with a capped delay.
- Steady-state session count per pipeline is bounded; a hard process kill
  still leaves sessions to expire server-side.

## Where in code

`voice-pipes/src/stt/assemblyai/` (`ws_client.rs`, `classify_server_error`),
`src/stt/google/client.rs` (rotation and reconnect).
Provider pages: [AssemblyAI](../../guides/providers/stt/assemblyai.md),
[Google](../../guides/providers/stt/google.md).
