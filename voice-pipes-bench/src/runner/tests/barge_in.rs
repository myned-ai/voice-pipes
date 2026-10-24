use super::*;

fn make_barge(
    interrupted: bool,
    trigger: Option<&str>,
    rtt: Option<u64>,
    recovery: Option<u64>,
    follow_up_completed: bool,
) -> BargeInResult {
    BargeInResult {
        interrupted,
        trigger: trigger.map(std::string::ToString::to_string),
        barge_in_rtt_ms: rtt,
        recovery_ms: recovery,
        follow_up_completed,
    }
}

#[test]
fn score_barge_in_flags_missing_interrupt() {
    let exp = ScenarioExpected {
        must_interrupt: true,
        ..Default::default()
    };
    let barge = make_barge(false, None, None, None, false);
    let mut failures = Vec::new();
    score_barge_in(&exp, &barge, &[], &mut failures);
    assert!(failures.iter().any(|f| f.contains("must_interrupt")));
}

#[test]
fn score_barge_in_flags_rtt_cap() {
    let exp = ScenarioExpected {
        must_interrupt: true,
        max_barge_in_rtt_ms: Some(100),
        ..Default::default()
    };
    let barge = make_barge(true, Some("client_cancel"), Some(250), None, false);
    let mut failures = Vec::new();
    score_barge_in(&exp, &barge, &[], &mut failures);
    assert!(failures.iter().any(|f| f.contains("max_barge_in_rtt_ms")));
}

#[test]
fn score_barge_in_flags_recovery_cap() {
    let exp = ScenarioExpected {
        must_interrupt: true,
        max_recovery_ms: Some(500),
        ..Default::default()
    };
    let barge = make_barge(true, Some("client_cancel"), Some(50), Some(900), false);
    let mut failures = Vec::new();
    score_barge_in(&exp, &barge, &[], &mut failures);
    assert!(failures.iter().any(|f| f.contains("max_recovery_ms")));
}

#[test]
fn score_barge_in_flags_wrong_trigger() {
    let exp = ScenarioExpected {
        must_interrupt: true,
        interruption_trigger: Some(ExpectedTrigger::ServerVad),
        ..Default::default()
    };
    let barge = make_barge(true, Some("client_cancel"), Some(50), None, false);
    let mut failures = Vec::new();
    score_barge_in(&exp, &barge, &[], &mut failures);
    assert!(failures.iter().any(|f| f.contains("interruption_trigger")));
}

#[test]
fn score_barge_in_flags_followup_incomplete() {
    let exp = ScenarioExpected {
        must_interrupt: true,
        follow_up_must_complete: true,
        ..Default::default()
    };
    let barge = make_barge(true, Some("client_cancel"), Some(50), Some(200), false);
    let mut failures = Vec::new();
    score_barge_in(&exp, &barge, &[], &mut failures);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("follow_up_must_complete"))
    );
}

fn turn_with_response(id: &str, response: Option<&str>) -> TurnResult {
    TurnResult {
        turn_id: id.to_string(),
        utterance_id: None,
        stt_text: None,
        response_text: response.map(std::string::ToString::to_string),
        e2e_latency_ms: Some(500.0),
        completed: true,
        agent_turns_seen: 0,
        expectation_failures: Vec::new(),
        error: None,
    }
}

/// `pipeline_must_say` distinguishes a real post-interrupt answer from
/// stray residue of the cancelled response (e.g. `ElevenLabs`' "Old...").
#[test]
fn score_barge_in_flags_missing_pipeline_must_say() {
    let exp = ScenarioExpected {
        must_interrupt: true,
        pipeline_must_say: vec!["Tokyo".to_string()],
        ..Default::default()
    };
    let barge = make_barge(true, Some("server_vad"), Some(50), Some(100), true);
    let turns = [turn_with_response("interrupt-reply", Some("Old..."))];
    let mut failures = Vec::new();
    score_barge_in(&exp, &barge, &turns, &mut failures);
    assert!(failures.iter().any(|f| f.contains("pipeline_must_say")));
}

#[test]
fn score_barge_in_pipeline_must_say_is_case_insensitive() {
    let exp = ScenarioExpected {
        pipeline_must_say: vec!["tokyo".to_string()],
        ..Default::default()
    };
    let barge = make_barge(true, Some("server_vad"), Some(50), Some(100), true);
    let turns = [turn_with_response(
        "interrupt-reply",
        Some("The capital of Japan is Tokyo."),
    )];
    let mut failures = Vec::new();
    score_barge_in(&exp, &barge, &turns, &mut failures);
    assert!(failures.is_empty(), "got failures: {failures:?}");
}

#[test]
fn score_barge_in_passes_when_within_budget() {
    let exp = ScenarioExpected {
        must_interrupt: true,
        interruption_trigger: Some(ExpectedTrigger::ClientCancel),
        max_barge_in_rtt_ms: Some(500),
        max_recovery_ms: Some(1500),
        follow_up_must_complete: true,
        ..Default::default()
    };
    let barge = make_barge(true, Some("client_cancel"), Some(120), Some(300), true);
    let mut failures = Vec::new();
    score_barge_in(&exp, &barge, &[], &mut failures);
    assert!(failures.is_empty(), "got failures: {failures:?}");
}

#[test]
fn trigger_for_mode_maps_all_variants() {
    use std::path::PathBuf;
    assert_eq!(
        trigger_for_mode(&InterruptMode::ClientCancel),
        "client_cancel"
    );
    assert_eq!(
        trigger_for_mode(&InterruptMode::InjectAudio {
            path: PathBuf::from("x.wav")
        }),
        "server_vad"
    );
    assert_eq!(
        trigger_for_mode(&InterruptMode::InjectText {
            text: "stop".into()
        }),
        "early_barge"
    );
}

#[test]
fn score_barge_in_accepts_server_vad_for_inject_audio() {
    let exp = ScenarioExpected {
        must_interrupt: true,
        interruption_trigger: Some(ExpectedTrigger::ServerVad),
        ..Default::default()
    };
    let barge = make_barge(true, Some("server_vad"), Some(80), Some(200), false);
    let mut failures = Vec::new();
    score_barge_in(&exp, &barge, &[], &mut failures);
    assert!(failures.is_empty(), "got failures: {failures:?}");
}

#[test]
fn score_barge_in_accepts_early_barge_for_inject_text() {
    let exp = ScenarioExpected {
        must_interrupt: true,
        interruption_trigger: Some(ExpectedTrigger::EarlyBarge),
        ..Default::default()
    };
    let barge = make_barge(true, Some("early_barge"), Some(40), Some(150), false);
    let mut failures = Vec::new();
    score_barge_in(&exp, &barge, &[], &mut failures);
    assert!(failures.is_empty(), "got failures: {failures:?}");
}
