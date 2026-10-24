#![expect(
    clippy::unnecessary_literal_bound,
    reason = "signatures are fixed by the ToolHandler trait, which supports dynamically named tools"
)]

use super::*;

use async_trait::async_trait;

struct EchoTool;

#[async_trait]
impl ToolHandler for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echo the input back verbatim."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        })
    }
    async fn invoke(&self, arguments: &str) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::Done(arguments.to_string()))
    }
}

/// Long-running tool shape: acks immediately, real result comes later
/// via `AgentEvent::CompleteTool`.
struct SlowTool;

#[async_trait]
impl ToolHandler for SlowTool {
    fn name(&self) -> &str {
        "slow"
    }
    fn description(&self) -> &str {
        "Kicks off a background task."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn invoke(&self, _arguments: &str) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::Pending {
            ack: serde_json::json!({
                "status": "working",
                "instruction": "briefly tell the user you're on it"
            })
            .to_string(),
        })
    }
}

#[test]
fn default_registry_is_empty() {
    let reg = default_tool_registry();
    assert!(reg.is_empty());
    assert!(reg.lookup("echo").is_none());
    assert!(reg.tool_specs().is_empty());
}

#[tokio::test]
async fn register_and_lookup_returns_last_match() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(EchoTool));
    let h = reg.lookup("echo").expect("echo tool registered");
    assert_eq!(h.name(), "echo");
    let out = h.invoke("{\"text\":\"hi\"}").await.expect("invoke ok");
    match out {
        ToolOutcome::Done(s) => assert_eq!(s, "{\"text\":\"hi\"}"),
        other @ ToolOutcome::Pending { .. } => panic!("expected Done, got {other:?}"),
    }
}

#[tokio::test]
async fn pending_outcome_carries_ack() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(SlowTool));
    let h = reg.lookup("slow").expect("slow tool registered");
    match h.invoke("{}").await.expect("invoke ok") {
        ToolOutcome::Pending { ack } => assert!(ack.contains("working")),
        other @ ToolOutcome::Done(_) => panic!("expected Pending, got {other:?}"),
    }
}

#[test]
fn tool_specs_round_trip_schema() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(EchoTool));
    let specs = reg.tool_specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "echo");
    assert_eq!(
        specs[0].parameters_json_schema["properties"]["text"]["type"],
        "string"
    );
}
