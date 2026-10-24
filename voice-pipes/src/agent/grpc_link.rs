//! tonic implementation of [`AgentLink`] + the `agent-grpc` text
//! processor factory.
//!
//! Wire schema: `proto/agent.proto` (the single protocol definition —
//! see `link.rs`). One `AgentSession` bidi stream per voice session;
//! HTTP/2 multiplexes many sessions over one worker↔agent connection.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tracing::warn;

use crate::agent::link::{AgentLink, AgentLinkError, AgentToPipeline, PipelineToAgent};
use crate::agent::{Scheduling, gateway::AgentGateway};
use crate::grpc::auth::{AuthedChannel, authed_channel};
use crate::text::{TextProcessor, TextProcessorError};

pub(crate) mod proto {
    #![allow(
        clippy::pedantic,
        clippy::nursery,
        reason = "tonic/prost generated code"
    )]
    #![allow(
        dead_code,
        clippy::all,
        clippy::pedantic,
        clippy::restriction,
        reason = "tonic-generated code"
    )]
    tonic::include_proto!("voice_pipes.agent");
}

type AgentClient = proto::agent_service_client::AgentServiceClient<AuthedChannel>;

/// Outbound queue depth. Turn text + control frames only (no audio);
/// a queue this deep means the agent stopped reading.
const OUT_QUEUE_CAPACITY: usize = 64;

/// Largest gRPC message accepted from the agent. Frames carry text and
/// control only; the per-field text cap is
/// [`MAX_AGENT_TEXT_BYTES`](crate::agent::link::MAX_AGENT_TEXT_BYTES),
/// so a message anywhere near this size is not a valid frame.
const MAX_INBOUND_MESSAGE_BYTES: usize = 1024 * 1024;

/// Ceiling on TCP/H2 connect and on opening the `AgentSession` stream.
/// The build runs during session (and worker) setup — without a bound,
/// an agent that accepts the connection but never sends response
/// headers (e.g. a Python server that defers initial metadata until
/// its first yield) hangs setup forever instead of failing loudly.
const DEFAULT_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// gRPC-backed [`AgentLink`]: one open `AgentSession` stream.
pub struct GrpcAgentLink {
    out_tx: mpsc::Sender<proto::PipelineToAgent>,
    inbound: Mutex<tonic::Streaming<proto::AgentToPipeline>>,
    client: AgentClient,
}

impl std::fmt::Debug for GrpcAgentLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcAgentLink").finish_non_exhaustive()
    }
}

impl GrpcAgentLink {
    /// Connect to the agent service and open the session stream, with
    /// [`DEFAULT_CONNECT_TIMEOUT`] on each step.
    pub async fn connect(endpoint: &str) -> Result<Self, anyhow::Error> {
        Self::connect_with_timeout(endpoint, DEFAULT_CONNECT_TIMEOUT).await
    }

    /// [`connect`](Self::connect) with an explicit per-step timeout.
    pub async fn connect_with_timeout(
        endpoint: &str,
        timeout: std::time::Duration,
    ) -> Result<Self, anyhow::Error> {
        let channel = tokio::time::timeout(
            timeout,
            Channel::from_shared(endpoint.to_string())?.connect(),
        )
        .await
        .map_err(|_elapsed| anyhow::anyhow!("gRPC agent connect timed out after {timeout:?}"))?
        .map_err(|e| anyhow::anyhow!("gRPC agent connect failed: {e}"))?;
        let mut client = AgentClient::new(authed_channel(channel))
            .max_decoding_message_size(MAX_INBOUND_MESSAGE_BYTES);

        let (out_tx, out_rx) = mpsc::channel::<proto::PipelineToAgent>(OUT_QUEUE_CAPACITY);
        let inbound = tokio::time::timeout(
            timeout,
            client.agent_session(tonic::Request::new(ReceiverStream::new(out_rx))),
        )
        .await
        .map_err(|_elapsed| {
            anyhow::anyhow!(
                "gRPC AgentSession open timed out after {timeout:?} — the agent accepted the \
                 connection but never sent response headers (a Python grpc server must call \
                 context.send_initial_metadata(()) before waiting for the first frame)"
            )
        })?
        .map_err(|e| anyhow::anyhow!("gRPC AgentSession open failed: {e}"))?
        .into_inner();

        Ok(Self {
            out_tx,
            inbound: Mutex::new(inbound),
            client,
        })
    }
}

#[async_trait::async_trait]
impl AgentLink for GrpcAgentLink {
    async fn send(&self, event: PipelineToAgent) -> Result<(), AgentLinkError> {
        self.out_tx
            .send(to_proto(event))
            .await
            .map_err(|_stream_gone| AgentLinkError::new("agent stream closed"))
    }

    async fn recv(&self) -> Option<AgentToPipeline> {
        loop {
            let next = self.inbound.lock().await.message().await;
            match next {
                Ok(Some(frame)) => {
                    if let Some(ev) = from_proto(frame) {
                        return Some(ev);
                    }
                    // Unknown/empty oneof from a newer agent: skip.
                }
                Ok(None) => return None,
                Err(status) => {
                    warn!(code = ?status.code(), message = %status.message(), "agent stream error");
                    return None;
                }
            }
        }
    }

    async fn list_tools(&self) -> Result<Vec<crate::agent::link::LinkToolSpec>, AgentLinkError> {
        let mut client = self.client.clone();
        let resp = client
            .list_tools(tonic::Request::new(proto::ListToolsRequest {}))
            .await
            .map_err(|e| AgentLinkError::new(format!("gRPC ListTools failed: {e}")))?
            .into_inner();
        Ok(resp
            .tools
            .into_iter()
            .map(|t| crate::agent::link::LinkToolSpec {
                name: t.name,
                description: t.description,
                parameters_json_schema: t.parameters_json_schema,
            })
            .collect())
    }

    fn close(&self, reason: &str) {
        let _ = self.out_tx.try_send(proto::PipelineToAgent {
            event: Some(proto::pipeline_to_agent::Event::SessionEnd(
                proto::SessionEnd {
                    reason: reason.to_string(),
                },
            )),
        });
    }
}

// ── enum ↔ proto mapping ────────────────────────────────────

fn scheduling_from_proto(s: i32) -> Scheduling {
    match proto::Scheduling::try_from(s) {
        Ok(proto::Scheduling::Interrupt) => Scheduling::Interrupt,
        Ok(proto::Scheduling::Silent) => Scheduling::Silent,
        // WhenIdle is both the proto default (= 0) and our default.
        _ => Scheduling::WhenIdle,
    }
}

fn to_proto(event: PipelineToAgent) -> proto::PipelineToAgent {
    use proto::pipeline_to_agent::Event;
    let event = match event {
        PipelineToAgent::SessionStart {
            protocol_version,
            session_id,
            pipeline_name,
            resumed,
        } => Event::SessionStart(proto::SessionStart {
            protocol_version,
            session_id,
            pipeline_name,
            resumed,
            metadata: std::collections::HashMap::new(),
        }),
        PipelineToAgent::UserTurn {
            turn_id,
            text,
            is_context,
        } => Event::UserTurn(proto::UserTurn {
            turn_id,
            text,
            is_context,
        }),
        PipelineToAgent::Interrupted {
            turn_id,
            played_text,
        } => Event::Interrupted(proto::Interrupted {
            turn_id,
            played_text,
        }),
        PipelineToAgent::ContextNote { text, kind } => {
            Event::ContextNote(proto::ContextNote { text, kind })
        }
        PipelineToAgent::UserIdle {
            idle_ms,
            prompt_count,
        } => Event::UserIdle(proto::UserIdle {
            idle_ms,
            prompt_count,
        }),
        PipelineToAgent::ToolCall {
            call_id,
            name,
            arguments_json,
        } => Event::ToolCall(proto::ToolCall {
            call_id,
            name,
            arguments_json,
        }),
        PipelineToAgent::SessionEnd { reason } => Event::SessionEnd(proto::SessionEnd { reason }),
    };
    proto::PipelineToAgent { event: Some(event) }
}

fn from_proto(frame: proto::AgentToPipeline) -> Option<AgentToPipeline> {
    use proto::agent_to_pipeline::Event;
    Some(match frame.event? {
        Event::SessionAck(a) => AgentToPipeline::SessionAck {
            protocol_version: a.protocol_version,
            agent_name: a.agent_name,
        },
        Event::TurnFragment(f) => AgentToPipeline::TurnFragment {
            turn_id: f.turn_id,
            text: f.text,
            is_end_of_response: f.is_end_of_response,
            first_token_ms: f.first_token_ms,
            token_count: f.token_count,
        },
        Event::Say(s) => AgentToPipeline::Say {
            text: s.text,
            scheduling: scheduling_from_proto(s.scheduling),
        },
        Event::Message(m) => AgentToPipeline::Message {
            kind: m.kind,
            text: (!m.text.is_empty()).then_some(m.text),
            payload_json: (!m.payload_json.is_empty()).then_some(m.payload_json),
            record_in_history: m.record_in_history,
        },
        Event::Cancel(_) => AgentToPipeline::Cancel,
        Event::ToolResult(r) => AgentToPipeline::ToolResult {
            call_id: r.call_id,
            pending: r.pending,
            output: r.output,
        },
        Event::CompleteTool(c) => AgentToPipeline::CompleteTool {
            call_id: c.call_id,
            result: c.result,
            scheduling: scheduling_from_proto(c.scheduling),
        },
    })
}

// ── Factory: `type = "agent-grpc"` text processor ───────────

/// Factory for the agent gateway: `[[text_processor]] type = "agent-grpc",
/// endpoint = "http://agent:50061"` (+ optional `turn_timeout_ms`).
#[derive(Debug)]
pub struct AgentGrpcFactory;

impl crate::text::TextProcessorFactory for AgentGrpcFactory {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "return type is fixed by the TextProcessorFactory trait signature"
    )]
    fn name(&self) -> &str {
        "agent-grpc"
    }

    fn is_local(&self) -> bool {
        false
    }

    fn validate(
        &self,
        cfg: &crate::config::TextProcessorConfig,
    ) -> Vec<crate::config::ConfigError> {
        let mut o = cfg.options();
        if o.string("endpoint").is_none() {
            o.error("endpoint", "agent-grpc requires `endpoint`");
        }
        let _ = o.optional_u64_in("turn_timeout_ms", 1..=3_600_000);
        o.into_errors()
    }

    fn build(
        &self,
        cfg: &crate::config::TextProcessorConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn TextProcessor>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let endpoint = cfg
            .options
            .get("endpoint")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("agent-grpc requires `endpoint` in options"))?;
        let turn_timeout = cfg
            .options
            .get("turn_timeout_ms")
            .and_then(serde_json::Value::as_u64)
            .map(std::time::Duration::from_millis);

        let rt = tokio::runtime::Handle::current();
        let link = tokio::task::block_in_place(|| rt.block_on(GrpcAgentLink::connect(endpoint)))?;
        let mut gateway = AgentGateway::new(Arc::new(link), "voice-pipes");
        if let Some(t) = turn_timeout {
            gateway = gateway.with_turn_timeout(t);
        }
        Ok(Box::new(gateway))
    }

    fn provider_schema(
        &self,
        cfg: &crate::config::TextProcessorConfig,
    ) -> crate::api::param_schema::ProviderSchema {
        use crate::api::param_schema::{ParamDescriptor, ProviderSchema};
        let mut s = ProviderSchema::default();
        if let Some(v) = cfg.options.get("endpoint") {
            s.push(
                ParamDescriptor::static_str(
                    "endpoint",
                    "Endpoint",
                    "URL of the remote agent service (AgentService gRPC).",
                ),
                v.clone(),
            );
        }
        s.passthrough_options(&cfg.options, &["endpoint"], "agent-grpc option.");
        s
    }
}

/// Map a link error into the text-processor error surface.
#[expect(dead_code, reason = "used by the tool registry")]
pub(crate) fn link_to_text_error(name: &str, e: &AgentLinkError) -> TextProcessorError {
    TextProcessorError::new(name.to_string(), format!("agent link error: {e}"))
}

// ── Tests: real tonic round trip ────────────────────────────

#[cfg(test)]
#[path = "tests/grpc_link.rs"]
mod tests;
