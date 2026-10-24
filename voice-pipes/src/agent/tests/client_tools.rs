use super::*;

fn tool(name: &str, speak: bool) -> ClientToolConfig {
    ClientToolConfig {
        name: name.into(),
        description: format!("test tool {name}"),
        parameters: default_parameters_schema(),
        speak_on_result: speak,
    }
}

#[test]
fn index_merge_last_source_wins_on_collision() {
    let inline = vec![tool("navigate", false), tool("highlight", false)];
    let file = vec![tool("navigate", true)];
    let runtime = vec![tool("highlight", true)];
    let index = ClientToolIndex::merge([inline.as_slice(), file.as_slice(), runtime.as_slice()]);
    assert_eq!(index.len(), 2);
    assert!(index.lookup("navigate").unwrap().speak_on_result);
    assert!(index.lookup("highlight").unwrap().speak_on_result);
}

/// A runtime-injected tool may not take a server tool's name; the
/// refusal is reported by name so the client can see it.
#[test]
fn runtime_tools_cannot_take_server_tool_names() {
    let server = ["issue_refund", "check_order"];
    let runtime = vec![
        tool("issue_refund", false),
        tool("navigate", false),
        tool("check_order", true),
    ];
    let (accepted, refused) = reject_reserved_names(runtime, |name| server.contains(&name));
    assert_eq!(
        accepted.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["navigate"]
    );
    assert_eq!(
        refused,
        vec!["check_order".to_string(), "issue_refund".to_string()]
    );
    let index = ClientToolIndex::merge([accepted.as_slice()]);
    assert!(index.lookup("issue_refund").is_none());
}

#[test]
fn options_tools_parse_and_skip_invalid() {
    let mut options = HashMap::new();
    options.insert(
        OPTIONS_KEY.to_string(),
        serde_json::json!([
            { "name": "play", "description": "Play media." },
            { "name": "", "description": "invalid: empty name" },
            { "description": "invalid: missing name" },
        ]),
    );
    let tools = parse_options_tools(&options);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "play");
    assert!(!tools[0].speak_on_result);
    assert_eq!(tools[0].parameters, serde_json::json!({"type": "object"}));
}

#[test]
fn options_tools_absent_or_non_array() {
    assert!(parse_options_tools(&HashMap::new()).is_empty());
    let mut options = HashMap::new();
    options.insert(OPTIONS_KEY.to_string(), serde_json::json!("nope"));
    assert!(parse_options_tools(&options).is_empty());
}

#[test]
fn validation_rejects_bad_names_and_schemas() {
    let mut t = tool("ok_name-1", false);
    assert!(t.validation_errors().is_empty());
    t.name = "spaces not allowed".into();
    assert!(!t.validation_errors().is_empty());
    t.name = "ok".into();
    t.parameters = serde_json::json!([1, 2]);
    assert!(!t.validation_errors().is_empty());
    t.parameters = serde_json::json!({"type": "object"});
    t.description = "  ".into();
    assert!(!t.validation_errors().is_empty());
}

#[test]
fn calls_cap_and_duplicate() {
    let mut calls = ClientToolCalls::new(2, Duration::from_mins(1));
    assert!(calls.try_register("c1", "navigate", false).is_ok());
    assert_eq!(
        calls.try_register("c1", "navigate", false),
        Err(RegisterError::DuplicateCall)
    );
    assert!(calls.try_register("c2", "navigate", false).is_ok());
    assert_eq!(
        calls.try_register("c3", "navigate", false),
        Err(RegisterError::Overloaded)
    );
    // Resolving frees a slot.
    assert!(calls.resolve("c1").is_some());
    assert!(calls.try_register("c3", "navigate", false).is_ok());
}

#[test]
fn calls_ttl_eviction_is_lazy() {
    let mut calls = ClientToolCalls::new(8, Duration::from_millis(0));
    assert!(calls.try_register("c1", "navigate", false).is_ok());
    std::thread::sleep(Duration::from_millis(5));
    // Expired entry is evicted on the next touch; its late result drops.
    assert!(calls.resolve("c1").is_none());
    assert!(calls.is_empty());
}

#[test]
fn resolve_result_completed_silent_and_spoken() {
    let mut calls = ClientToolCalls::default();
    calls.try_register("c1", "navigate", false).unwrap();
    let d = resolve_result(
        &mut calls,
        "c1",
        ClientToolPhase::Completed,
        Some(&serde_json::json!({"screen": "settings"})),
        false,
    );
    match d {
        ResultDisposition::Context { context, speak } => {
            assert!(!speak, "speak_on_result=false and no client request");
            assert!(context.contains("navigate"));
            assert!(context.contains("settings"));
            assert!(context.contains("not instructions"));
        }
        other => panic!("expected Context, got {other:?}"),
    }
    // Duplicate Completed after the first is a no-op.
    let dup = resolve_result(&mut calls, "c1", ClientToolPhase::Completed, None, false);
    assert_eq!(dup, ResultDisposition::Ignore);

    // speak_on_result=true forces speech.
    calls.try_register("c2", "confirm_order", true).unwrap();
    match resolve_result(&mut calls, "c2", ClientToolPhase::Completed, None, false) {
        ResultDisposition::Context { speak, .. } => assert!(speak),
        other => panic!("expected Context, got {other:?}"),
    }

    // The client's own event can request speech.
    calls.try_register("c3", "navigate", false).unwrap();
    match resolve_result(&mut calls, "c3", ClientToolPhase::Completed, None, true) {
        ResultDisposition::Context { speak, .. } => assert!(speak),
        other => panic!("expected Context, got {other:?}"),
    }
}

#[test]
fn resolve_result_progress_keeps_call_in_flight() {
    let mut calls = ClientToolCalls::default();
    calls.try_register("c1", "navigate", false).unwrap();
    assert_eq!(
        resolve_result(&mut calls, "c1", ClientToolPhase::Started, None, false),
        ResultDisposition::Acked,
        "a known call's interim phase is an ack (recorded for diagnostics)"
    );
    assert_eq!(
        resolve_result(&mut calls, "c1", ClientToolPhase::Progress, None, false),
        ResultDisposition::Acked
    );
    assert_eq!(calls.len(), 1, "interim phases must not resolve the call");
    assert_eq!(
        resolve_result(&mut calls, "unknown", ClientToolPhase::Started, None, false),
        ResultDisposition::Ignore,
        "unknown ids stay dropped"
    );
}

#[test]
fn resolve_result_failed_notes_failure() {
    let mut calls = ClientToolCalls::default();
    calls.try_register("c1", "navigate", false).unwrap();
    match resolve_result(
        &mut calls,
        "c1",
        ClientToolPhase::Failed,
        Some(&serde_json::json!({"error": "screen not found"})),
        false,
    ) {
        ResultDisposition::Context { context, .. } => {
            assert!(context.contains("FAILED"));
            assert!(context.contains("screen not found"));
        }
        other => panic!("expected Context, got {other:?}"),
    }
}

#[test]
fn result_context_caps_oversized_payloads() {
    let mut calls = ClientToolCalls::default();
    calls.try_register("c1", "dump", false).unwrap();
    let big = serde_json::Value::String("x".repeat(MAX_RESULT_BYTES * 2));
    match resolve_result(
        &mut calls,
        "c1",
        ClientToolPhase::Completed,
        Some(&big),
        false,
    ) {
        ResultDisposition::Context { context, .. } => {
            assert!(context.len() < MAX_RESULT_BYTES + 512);
            assert!(context.contains("[truncated]"));
        }
        other => panic!("expected Context, got {other:?}"),
    }
}

#[test]
fn trigger_action_message_shape() {
    let msg = trigger_action_message(
        "c1",
        "navigate",
        serde_json::json!({"screen": "home"}),
        true,
    );
    assert_eq!(msg.kind, "trigger_action");
    let payload = msg.payload.expect("payload");
    assert_eq!(payload["call_id"], "c1");
    assert_eq!(payload["name"], "navigate");
    assert_eq!(payload["arguments"]["screen"], "home");
    assert_eq!(payload["speak_on_result"], true);
}

#[test]
fn catalog_file_json_and_toml() {
    let dir = std::env::temp_dir().join(format!("vp-ct-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    let json_path = dir.join("tools.json");
    std::fs::write(
        &json_path,
        r#"[{ "name": "highlight", "description": "Highlight a UI element.",
             "parameters": { "type": "object",
               "properties": { "selector": { "type": "string" } },
               "required": ["selector"] } }]"#,
    )
    .unwrap();
    let tools = load_catalog_file(&json_path).expect("json catalog");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "highlight");

    let toml_path = dir.join("tools.toml");
    std::fs::write(
        &toml_path,
        "[[client_tool]]\nname = \"navigate\"\ndescription = \"Go to a screen.\"\n",
    )
    .unwrap();
    let tools = load_catalog_file(&toml_path).expect("toml catalog");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "navigate");

    // Missing file and invalid entries are hard errors (fail fast).
    assert!(load_catalog_file(&dir.join("missing.json")).is_err());
    let bad = dir.join("bad.json");
    std::fs::write(&bad, r#"[{ "name": "", "description": "x" }]"#).unwrap();
    assert!(load_catalog_file(&bad).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

/// The realtime runner knows which turn dispatched a call (on Gemini 3.x a
/// turn with no audio at all) and records it; the cascade leaves it out.
#[test]
fn call_audit_payload_names_the_owning_turn_when_known() {
    let args = serde_json::json!({ "title": "x" });
    let with = call_audit_payload("c1", "play_media", &args, true, Some(2));
    assert_eq!(with["utterance_id"], 2);
    assert_eq!(with["speak_on_result"], true);
    let without = call_audit_payload("c1", "play_media", &args, true, None);
    assert!(without.get("utterance_id").is_none());
}

/// A spoken failure that carries a contradicting payload (the Playground's
/// untouched Fail used to send {"status": "ok"}) still tells the model the
/// action did not work; a spoken success keeps the neutral wording.
#[test]
fn spoken_failure_overrides_a_contradicting_payload() {
    let mut calls = ClientToolCalls::default();
    calls.try_register("c1", "play_media", true).unwrap();
    calls.try_register("c2", "play_media", true).unwrap();
    let ok = serde_json::json!({"status": "ok"});
    match resolve_result(&mut calls, "c1", ClientToolPhase::Failed, Some(&ok), false) {
        ResultDisposition::Context { context, speak } => {
            assert!(speak);
            assert!(context.contains("FAILED"));
            assert!(context.contains("did not succeed, whatever the data says"));
        }
        other => panic!("expected Context, got {other:?}"),
    }
    match resolve_result(
        &mut calls,
        "c2",
        ClientToolPhase::Completed,
        Some(&ok),
        false,
    ) {
        ResultDisposition::Context { context, .. } => {
            assert!(context.contains("Briefly tell the user the outcome."));
            assert!(!context.contains("did not succeed"));
        }
        other => panic!("expected Context, got {other:?}"),
    }
}
