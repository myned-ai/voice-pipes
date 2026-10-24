use super::*;

fn turn(id: &str, latency: Option<f64>, completed: bool) -> TurnResult {
    TurnResult {
        agent_turns_seen: 0,
        turn_id: id.into(),
        utterance_id: None,
        stt_text: None,
        response_text: None,
        e2e_latency_ms: latency,
        completed,
        expectation_failures: Vec::new(),
        error: None,
    }
}

#[test]
fn score_conversation_flags_missing_turns() {
    let exp = ScenarioExpected {
        turns_completed: Some(2),
        ..Default::default()
    };
    let turns = vec![turn("t1", Some(100.0), true), turn("t2", None, false)];
    let mut failures = Vec::new();
    score_conversation(&exp, &turns, &mut failures);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("turns_completed"));
}

#[test]
fn score_conversation_flags_per_turn_latency_cap() {
    let exp = ScenarioExpected {
        max_latency_ms_per_turn: Some(500),
        ..Default::default()
    };
    let turns = vec![turn("t1", Some(200.0), true), turn("t2", Some(800.0), true)];
    let mut failures = Vec::new();
    score_conversation(&exp, &turns, &mut failures);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("turn t2"));
}

#[test]
fn score_conversation_flags_latency_drift() {
    // Base 100ms, allow +25% => 125ms cap. t2 at 200ms must fail.
    let exp = ScenarioExpected {
        max_latency_drift_pct: Some(25.0),
        ..Default::default()
    };
    let turns = vec![turn("t1", Some(100.0), true), turn("t2", Some(200.0), true)];
    let mut failures = Vec::new();
    score_conversation(&exp, &turns, &mut failures);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("drift"));
}

#[test]
fn score_conversation_passes_when_within_budget() {
    let exp = ScenarioExpected {
        turns_completed: Some(2),
        max_latency_ms_per_turn: Some(500),
        max_latency_drift_pct: Some(50.0),
        ..Default::default()
    };
    let turns = vec![turn("t1", Some(200.0), true), turn("t2", Some(250.0), true)];
    let mut failures = Vec::new();
    score_conversation(&exp, &turns, &mut failures);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");
}

#[test]
fn apply_turn_assertions_flags_missing_substring() {
    let conv = ConversationTurn {
        expect_followups: 0,
        followup_timeout_ms: None,
        id: "t1".into(),
        input: TurnInput::Text { text: "hi".into() },
        expected: None,
    };
    let exp = TurnExpected {
        transcript_contains: vec!["weather".into()],
        ..Default::default()
    };
    let mut result = TurnResult {
        agent_turns_seen: 0,
        turn_id: "t1".into(),
        utterance_id: None,
        stt_text: Some("hello there".into()),
        response_text: None,
        e2e_latency_ms: Some(200.0),
        completed: true,
        expectation_failures: Vec::new(),
        error: None,
    };
    apply_turn_assertions(&conv, &exp, &mut result);
    assert_eq!(result.expectation_failures.len(), 1);
}

#[test]
fn apply_turn_assertions_passes_case_insensitive_substring() {
    let conv = ConversationTurn {
        expect_followups: 0,
        followup_timeout_ms: None,
        id: "t1".into(),
        input: TurnInput::Text { text: "hi".into() },
        expected: None,
    };
    let exp = TurnExpected {
        transcript_contains: vec!["HELLO".into()],
        ..Default::default()
    };
    let mut result = TurnResult {
        agent_turns_seen: 0,
        turn_id: "t1".into(),
        utterance_id: None,
        stt_text: Some("hello there".into()),
        response_text: None,
        e2e_latency_ms: None,
        completed: true,
        expectation_failures: Vec::new(),
        error: None,
    };
    apply_turn_assertions(&conv, &exp, &mut result);
    assert!(result.expectation_failures.is_empty());
}
