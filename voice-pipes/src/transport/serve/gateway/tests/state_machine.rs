use super::*;

use crate::pipeline::new_turn_state;

// ── evaluate(): the full transition table ───────────────────

#[test]
fn utterance_commit_moves_listening_to_processing() {
    assert_eq!(
        evaluate(TurnState::Listening, TurnTrigger::UtteranceCommitted),
        TurnTransition::To(TurnState::Processing)
    );
}

#[test]
fn utterance_commit_is_noop_when_already_processing() {
    assert_eq!(
        evaluate(TurnState::Processing, TurnTrigger::UtteranceCommitted),
        TurnTransition::NoChange
    );
}

#[test]
fn utterance_commit_is_rejected_while_speaking() {
    assert_eq!(
        evaluate(TurnState::Speaking, TurnTrigger::UtteranceCommitted),
        TurnTransition::Rejected
    );
}

/// The previous turn is finished on the server while its tail still plays
/// on the client; a new utterance starts the next turn from there.
#[test]
fn utterance_commit_from_draining_starts_the_next_turn() {
    assert_eq!(
        evaluate(TurnState::Draining, TurnTrigger::UtteranceCommitted),
        TurnTransition::To(TurnState::Processing)
    );
}

#[test]
fn apply_verdict_reports_rejections_without_moving() {
    let state = new_turn_state(); // Listening
    assert_eq!(
        apply_verdict(&state, TurnTrigger::PlaybackFinalized),
        TurnTransition::Rejected
    );
    assert_eq!(get_turn_state(&state), TurnState::Listening);
    assert_eq!(
        apply_verdict(&state, TurnTrigger::UtteranceCommitted),
        TurnTransition::To(TurnState::Processing)
    );
    assert_eq!(
        apply_verdict(&state, TurnTrigger::UtteranceCommitted),
        TurnTransition::NoChange
    );
    assert_eq!(get_turn_state(&state), TurnState::Processing);
}

#[test]
fn first_audio_moves_processing_to_speaking() {
    assert_eq!(
        evaluate(TurnState::Processing, TurnTrigger::AudioOutputStarted),
        TurnTransition::To(TurnState::Speaking)
    );
}

#[test]
fn first_audio_is_noop_when_already_speaking() {
    assert_eq!(
        evaluate(TurnState::Speaking, TurnTrigger::AudioOutputStarted),
        TurnTransition::NoChange
    );
}

#[test]
fn audio_flips_to_speaking_from_any_non_speaking_state() {
    assert_eq!(
        evaluate(TurnState::Listening, TurnTrigger::AudioOutputStarted),
        TurnTransition::To(TurnState::Speaking)
    );
    assert_eq!(
        evaluate(TurnState::Draining, TurnTrigger::AudioOutputStarted),
        TurnTransition::To(TurnState::Speaking)
    );
}

#[test]
fn draining_trigger_moves_active_states_to_draining() {
    for from in [
        TurnState::Listening,
        TurnState::Processing,
        TurnState::Speaking,
    ] {
        assert_eq!(
            evaluate(from, TurnTrigger::Draining),
            TurnTransition::To(TurnState::Draining)
        );
    }
}

#[test]
fn draining_trigger_is_noop_when_already_draining() {
    assert_eq!(
        evaluate(TurnState::Draining, TurnTrigger::Draining),
        TurnTransition::NoChange
    );
}

#[test]
fn playback_finalized_returns_to_listening_from_playback_states() {
    assert_eq!(
        evaluate(TurnState::Speaking, TurnTrigger::PlaybackFinalized),
        TurnTransition::To(TurnState::Listening)
    );
    assert_eq!(
        evaluate(TurnState::Draining, TurnTrigger::PlaybackFinalized),
        TurnTransition::To(TurnState::Listening)
    );
}

#[test]
fn playback_finalized_is_rejected_outside_playback_states() {
    // `handle_playback_complete` warned-and-ignored these.
    assert_eq!(
        evaluate(TurnState::Listening, TurnTrigger::PlaybackFinalized),
        TurnTransition::Rejected
    );
    assert_eq!(
        evaluate(TurnState::Processing, TurnTrigger::PlaybackFinalized),
        TurnTransition::Rejected
    );
}

// ── apply(): atomic side effects ────────────────────────────

#[test]
fn apply_changes_state_and_reports_new_state() {
    let state = new_turn_state();
    let result = apply(&state, TurnTrigger::UtteranceCommitted);
    assert_eq!(result, Some(TurnState::Processing));
    assert_eq!(get_turn_state(&state), TurnState::Processing);
}

#[test]
fn apply_returns_none_and_leaves_state_on_noop() {
    let state = new_turn_state();
    apply(&state, TurnTrigger::UtteranceCommitted); // -> Processing
    apply(&state, TurnTrigger::AudioOutputStarted); // -> Speaking
    let result = apply(&state, TurnTrigger::AudioOutputStarted); // no-op
    assert_eq!(result, None);
    assert_eq!(get_turn_state(&state), TurnState::Speaking);
}

#[test]
fn apply_returns_none_and_leaves_state_on_rejected() {
    let state = new_turn_state(); // Listening
    let result = apply(&state, TurnTrigger::PlaybackFinalized);
    assert_eq!(result, None);
    assert_eq!(get_turn_state(&state), TurnState::Listening);
}

#[test]
fn full_happy_path_cycle() {
    let state = new_turn_state();
    assert_eq!(get_turn_state(&state), TurnState::Listening);
    assert_eq!(
        apply(&state, TurnTrigger::UtteranceCommitted),
        Some(TurnState::Processing)
    );
    assert_eq!(
        apply(&state, TurnTrigger::AudioOutputStarted),
        Some(TurnState::Speaking)
    );
    assert_eq!(
        apply(&state, TurnTrigger::Draining),
        Some(TurnState::Draining)
    );
    assert_eq!(
        apply(&state, TurnTrigger::PlaybackFinalized),
        Some(TurnState::Listening)
    );
}

// ── AgentTurnStarted: agent-initiated turns ────────

#[test]
fn agent_turn_moves_listening_to_processing() {
    assert_eq!(
        evaluate(TurnState::Listening, TurnTrigger::AgentTurnStarted),
        TurnTransition::To(TurnState::Processing)
    );
}

#[test]
fn agent_turn_is_rejected_from_every_non_listening_state() {
    // Stricter than UtteranceCommitted: the runner queues agent events
    // while a turn is active, so these are upstream bugs, not races.
    for state in [
        TurnState::Processing,
        TurnState::Speaking,
        TurnState::Draining,
    ] {
        assert_eq!(
            evaluate(state, TurnTrigger::AgentTurnStarted),
            TurnTransition::Rejected,
            "AgentTurnStarted from {state:?} must be rejected"
        );
    }
}
