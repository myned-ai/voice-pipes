use super::*;
use crate::pipeline::{new_turn_state, set_turn_state};

#[test]
fn barge_in_gating_covers_every_turn_state() {
    let settings = BargeInSettings::default();

    let all_states = [
        TurnState::Listening,
        TurnState::Processing,
        TurnState::Speaking,
        TurnState::Draining,
    ];

    for state in all_states {
        match state {
            TurnState::Listening => {
                assert!(!should_evaluate_barge_in(state, &settings));
            }
            TurnState::Processing => {
                settings.set_barge_in_during_processing(false);
                assert!(!should_evaluate_barge_in(state, &settings));

                settings.set_barge_in_during_processing(true);
                assert!(should_evaluate_barge_in(state, &settings));
            }
            TurnState::Speaking | TurnState::Draining => {
                assert!(should_evaluate_barge_in(state, &settings));
            }
        }
    }
}

#[test]
fn rms_engine_exposes_component() {
    let (_tx, rx) = mpsc::channel::<AudioChunk>(1);
    let engine = RmsVadEngine::new(rx, new_turn_state(), BargeInSettings::default());
    let c = engine.as_component().unwrap();
    assert_eq!(c.id(), "vad:rms");
    assert_eq!(c.kind(), ComponentKind::Vad);
}

#[test]
fn composite_engine_exposes_component() {
    let engine = CompositeVadEngine::new(vec![]);
    let c = engine.as_component().unwrap();
    assert_eq!(c.id(), "vad:composite");
}

// ── ProviderVadEngine via SpeechSignalSource (no STT dependency) ────

struct StaticSource(Vec<SpeechSignal>);

#[async_trait]
impl SpeechSignalSource for StaticSource {
    async fn next(&mut self) -> Option<SpeechSignal> {
        if self.0.is_empty() {
            None
        } else {
            Some(self.0.remove(0))
        }
    }
}

#[tokio::test]
async fn provider_engine_emits_when_signal_has_text_and_state_speaking() {
    let state = new_turn_state();
    set_turn_state(&state, TurnState::Speaking);

    let source = Box::new(StaticSource(vec![SpeechSignal {
        speech_started: false,
        has_text: true,
    }]));
    let mut engine = ProviderVadEngine::from_source(source, state, BargeInSettings::default());

    let (tx, mut rx) = mpsc::channel::<VadEvent>(4);
    engine.start(tx).await;

    let evt = rx.recv().await.expect("expected SpeechStarted");
    assert!(matches!(evt, VadEvent::SpeechStarted { .. }));
}

#[tokio::test]
async fn provider_engine_silent_when_listening() {
    let state = new_turn_state(); // defaults to Listening
    let source = Box::new(StaticSource(vec![SpeechSignal {
        speech_started: true,
        has_text: false,
    }]));
    let mut engine = ProviderVadEngine::from_source(source, state, BargeInSettings::default());

    let (tx, mut rx) = mpsc::channel::<VadEvent>(4);
    engine.start(tx).await;

    // Source exhausted, channel still open but no event was sent.
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn invented_text_cannot_interrupt_when_a_local_vad_is_judging_the_audio() {
    // A recogniser that hallucinates from near-silence produced interim text
    // for a sound far too quiet to cross the energy gate, and interrupted the
    // agent with it three times in one session.
    let state = new_turn_state();
    set_turn_state(&state, TurnState::Speaking);

    let source = Box::new(StaticSource(vec![SpeechSignal {
        speech_started: false,
        has_text: true,
    }]));
    let mut engine = ProviderVadEngine::from_source(source, state, BargeInSettings::default())
        .text_implies_speech(false);

    let (tx, mut rx) = mpsc::channel::<VadEvent>(4);
    engine.start(tx).await;
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn an_explicit_speech_start_always_interrupts() {
    let state = new_turn_state();
    set_turn_state(&state, TurnState::Speaking);

    let source = Box::new(StaticSource(vec![SpeechSignal {
        speech_started: true,
        has_text: false,
    }]));
    let mut engine = ProviderVadEngine::from_source(source, state, BargeInSettings::default())
        .text_implies_speech(false);

    let (tx, mut rx) = mpsc::channel::<VadEvent>(4);
    engine.start(tx).await;
    assert!(matches!(
        rx.recv().await,
        Some(VadEvent::SpeechStarted { .. })
    ));
}
