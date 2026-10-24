//! Centralized, validated turn-state machine.
//!
//! This module is the single source of truth for [`TurnState`] transitions:
//! it encodes the legal transitions as an exhaustive `match` so that adding a
//! new [`TurnState`] variant forces every transition to be reconsidered at
//! compile time, and illegal transitions are rejected (and observable)
//! instead of being stored silently.

use crate::pipeline::{SharedTurnState, TurnState, get_turn_state, set_turn_state};

/// Events that can drive a turn-state transition.
///
/// Triggers are named for *what happened*, not for the target state, so the
/// state machine — not the call site — decides the resulting state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnTrigger {
    /// A committed utterance entered processing (STT-final -> LLM/TTS).
    UtteranceCommitted,
    /// An agent-initiated turn began (no preceding user utterance).
    /// Legal only from Listening — the runner queues agent events
    /// while a turn is active, so any other state is a bug upstream.
    AgentTurnStarted,
    /// Assistant audio output began streaming to the client.
    AudioOutputStarted,
    /// The active turn is being torn down (cancel, barge-in, or normal end).
    Draining,
    /// Playback finished and the turn is finalized (client ack or the
    /// server-side short-circuit for early barge-in).
    PlaybackFinalized,
}

/// Outcome of evaluating a [`TurnTrigger`] against the current [`TurnState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnTransition {
    /// The trigger moves the machine to the given state.
    To(TurnState),
    /// The trigger is a no-op in the current state (idempotent); allowed.
    NoChange,
    /// The trigger is illegal from the current state and must be ignored.
    Rejected,
}

/// Evaluate `trigger` against `from`, returning the validated transition
/// without mutating any state.
///
/// The `match` is exhaustive over every `(TurnState, TurnTrigger)` pair so a
/// new variant on either enum is a compile error until handled here.
#[expect(
    clippy::match_same_arms,
    reason = "the transition table deliberately spells out every \
              (state, trigger) pair on its own line so each policy is \
              individually documented and reviewable"
)]
pub(crate) fn evaluate(from: TurnState, trigger: TurnTrigger) -> TurnTransition {
    use TurnState::{Draining, Listening, Processing, Speaking};
    use TurnTransition::{NoChange, Rejected, To};

    match (from, trigger) {
        // ── Listening -> Processing when an utterance is committed ──
        // Draining also accepts a commit: the previous turn is finished on
        // the server and only its tail is still playing on the client, so a
        // new utterance (typically typed text, or speech after an interrupt)
        // starts the next turn without waiting for `PlaybackComplete`.
        // Speaking does not: audio for the current turn is still being
        // produced, and a barge-in must tear it down first.
        (Listening, TurnTrigger::UtteranceCommitted) => To(Processing),
        (Processing, TurnTrigger::UtteranceCommitted) => NoChange,
        (Speaking, TurnTrigger::UtteranceCommitted) => Rejected,
        (Draining, TurnTrigger::UtteranceCommitted) => To(Processing),

        // ── Listening -> Processing when the agent opens a turn ──
        // Stricter than UtteranceCommitted: the runner must only
        // dispatch agent turns while idle (queued otherwise), so a
        // non-Listening state is always an upstream bug.
        (Listening, TurnTrigger::AgentTurnStarted) => To(Processing),
        (Processing, TurnTrigger::AgentTurnStarted) => Rejected,
        (Speaking, TurnTrigger::AgentTurnStarted) => Rejected,
        (Draining, TurnTrigger::AgentTurnStarted) => Rejected,

        // ── First assistant audio chunk -> Speaking ──
        // Flips to Speaking from any other state; a no-op if already Speaking.
        (Speaking, TurnTrigger::AudioOutputStarted) => NoChange,
        (Listening, TurnTrigger::AudioOutputStarted) => To(Speaking),
        (Processing, TurnTrigger::AudioOutputStarted) => To(Speaking),
        (Draining, TurnTrigger::AudioOutputStarted) => To(Speaking),

        // ── Teardown -> Draining ──
        // Teardown is idempotent: an already-Draining machine is a no-op.
        (Draining, TurnTrigger::Draining) => NoChange,
        (Listening, TurnTrigger::Draining) => To(Draining),
        (Processing, TurnTrigger::Draining) => To(Draining),
        (Speaking, TurnTrigger::Draining) => To(Draining),

        // ── Playback finalized -> Listening, only from playback states ──
        // `handle_playback_complete` explicitly warned-and-ignored this
        // trigger from Listening/Processing; that becomes `Rejected` here.
        (Speaking, TurnTrigger::PlaybackFinalized) => To(Listening),
        (Draining, TurnTrigger::PlaybackFinalized) => To(Listening),
        (Listening, TurnTrigger::PlaybackFinalized) => Rejected,
        (Processing, TurnTrigger::PlaybackFinalized) => Rejected,
    }
}

/// Apply `trigger` to the shared atomic state.
///
/// Returns `Some(new_state)` when the state actually changed, and `None`
/// for an idempotent no-op or a rejected (illegal) transition. The runner
/// uses [`apply_verdict`] so it can log rejections; this shorthand stays for
/// the transition-table tests.
#[cfg(test)]
pub(crate) fn apply(state: &SharedTurnState, trigger: TurnTrigger) -> Option<TurnState> {
    match apply_verdict(state, trigger) {
        TurnTransition::To(to) => Some(to),
        TurnTransition::NoChange | TurnTransition::Rejected => None,
    }
}

/// Apply `trigger` to the shared atomic state and hand back the verdict, so
/// the caller can tell an idempotent repeat (`NoChange`) from an illegal
/// trigger (`Rejected`) and log the latter. The state moves only on `To`.
///
/// # Invariant (single writer)
///
/// This is a non-atomic read-modify-write (`get` then `set`), so it is only
/// correct because **`turn_state` has exactly one writer task** — the session
/// runner (`serve::runner::context` and the `serve::turn` helpers it calls).
/// Concurrent readers (e.g. the VAD engine's `should_evaluate_barge_in`) only
/// `get_turn_state`, which is a coherent atomic load. Do NOT call this from a
/// second task: two interleaved read-modify-writes would drop a transition.
/// If a second writer ever becomes necessary, move `turn_state` behind
/// a `tokio::sync::watch` or a mutex first.
pub(crate) fn apply_verdict(state: &SharedTurnState, trigger: TurnTrigger) -> TurnTransition {
    let from = get_turn_state(state);
    let verdict = evaluate(from, trigger);
    if let TurnTransition::To(to) = verdict {
        set_turn_state(state, to);
    }
    verdict
}

#[cfg(test)]
#[path = "tests/state_machine.rs"]
mod tests;
