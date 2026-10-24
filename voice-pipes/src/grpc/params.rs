//! gRPC server + client for the runtime parameter editor.
//!
//! Hosts a `ParamService` that dispatches `UpdateParams` calls through a
//! shared [`ParamRegistry`]. The client side wraps the generated stub in a
//! [`RemoteParamStore`] that implements [`MutableParamStore`], so the
//! orchestrator's local registry can transparently include components that
//! live in a different process.

use std::sync::Arc;

use async_trait::async_trait;
use tonic::transport::Channel;
use tonic::{Request, Response, Status};

use crate::api::param_schema::{ParamError, ParamValue};
use crate::api::param_store::{MutableParamStore, ParamRegistry};

/// Auto-generated gRPC stubs for the parameter service.
pub(crate) mod proto {
    #![allow(
        dead_code,
        clippy::all,
        clippy::pedantic,
        clippy::restriction,
        reason = "tonic-generated code"
    )]
    tonic::include_proto!("voice_pipes.params");
}

pub use proto::param_service_client::ParamServiceClient;
pub use proto::param_service_server::ParamServiceServer;

// ── Conversions between domain ParamValue and proto ─────────

#[expect(
    clippy::result_large_err,
    reason = "tonic::Status is the framework error type on this boundary"
)]
fn proto_to_param_value(v: &proto::ParamValue) -> Result<ParamValue, Status> {
    match v.kind.as_ref() {
        Some(proto::param_value::Kind::FloatVal(f)) => Ok(ParamValue::Float(*f)),
        Some(proto::param_value::Kind::IntVal(i)) => Ok(ParamValue::Int(*i)),
        Some(proto::param_value::Kind::BoolVal(b)) => Ok(ParamValue::Bool(*b)),
        Some(proto::param_value::Kind::StrVal(s)) => Ok(ParamValue::Str(s.clone())),
        None => Err(Status::invalid_argument("ParamValue.kind missing")),
    }
}

fn param_value_to_proto(v: &ParamValue) -> proto::ParamValue {
    // A value kind the wire does not carry goes without one, which the
    // receiver refuses as a missing value.
    let kind = match v {
        ParamValue::Float(f) => Some(proto::param_value::Kind::FloatVal(*f)),
        ParamValue::Int(i) => Some(proto::param_value::Kind::IntVal(*i)),
        ParamValue::Bool(b) => Some(proto::param_value::Kind::BoolVal(*b)),
        ParamValue::Str(s) => Some(proto::param_value::Kind::StrVal(s.clone())),
        _ => None,
    };
    proto::ParamValue { kind }
}

fn param_value_to_json(v: &ParamValue) -> serde_json::Value {
    match v {
        ParamValue::Float(f) => serde_json::json!(f),
        ParamValue::Int(i) => serde_json::json!(i),
        ParamValue::Bool(b) => serde_json::json!(b),
        ParamValue::Str(s) => serde_json::json!(s),
        _ => serde_json::Value::Null,
    }
}

fn param_error_to_status(e: ParamError) -> Status {
    match e {
        ParamError::UnknownComponent(_) | ParamError::UnknownKey(_) => {
            Status::not_found(e.to_string())
        }
        ParamError::Immutable
        | ParamError::TypeMismatch { .. }
        | ParamError::OutOfRange { .. }
        | ParamError::NotInOptions(_)
        | ParamError::TooLong { .. } => Status::invalid_argument(e.to_string()),
        // A remote failure, and any condition added later.
        _ => Status::internal(e.to_string()),
    }
}

// ── Server ──────────────────────────────────────────────────

/// gRPC server for the parameter editor. Holds an `Arc<ParamRegistry>`
/// shared with the rest of the component-server's services so an
/// `UpdateParams` call directly mutates the same atomic cells that the
/// hot-path components read from.
#[derive(Debug)]
pub struct ParamServiceImpl {
    registry: Arc<ParamRegistry>,
}

impl ParamServiceImpl {
    pub fn new(registry: Arc<ParamRegistry>) -> Self {
        Self { registry }
    }
}

#[tonic::async_trait]
impl proto::param_service_server::ParamService for ParamServiceImpl {
    async fn update_params(
        &self,
        request: Request<proto::UpdateParamsRequest>,
    ) -> Result<Response<proto::UpdateParamsResponse>, Status> {
        let req = request.into_inner();
        let value = req
            .value
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("UpdateParamsRequest.value missing"))?;
        let typed = proto_to_param_value(value)?;
        // Re-encode through JSON so the existing schema-driven validation
        // applies (range checks, options, etc.).
        let json_val = param_value_to_json(&typed);
        let applied = self
            .registry
            .apply(&req.component_id, &req.key, json_val)
            .await
            .map_err(param_error_to_status)?;

        Ok(Response::new(proto::UpdateParamsResponse {
            applied: Some(param_value_to_proto(&applied)),
        }))
    }
}

// ── Shared schema-mapping helpers ────────────────
// One copy for the three component servers; the generated param proto
// types live here, so the mapping does too. Gated on `component-server`
// like their only callers (stt/tts/text_server) — this module itself
// compiles whenever ANY gRPC feature is on (e.g. `text-grpc`), where
// these would otherwise be dead code.

#[cfg(feature = "component-server")]
pub(crate) fn param_descriptor_to_proto(
    d: &crate::api::param_schema::ParamDescriptor,
) -> proto::ParamDescriptor {
    let value_type = match d.value_type {
        crate::api::param_schema::ParamValueType::Float => "float",
        crate::api::param_schema::ParamValueType::Int => "int",
        crate::api::param_schema::ParamValueType::Bool => "bool",
        crate::api::param_schema::ParamValueType::Path => "path",
        crate::api::param_schema::ParamValueType::Enum => "enum",
        // Str, and any type added later: the receiver reads a name it
        // does not know as a string.
        _ => "str",
    }
    .to_string();

    let constraints = d.constraints.as_ref().map(|c| proto::ParamConstraints {
        min: c.min,
        max: c.max,
        options: c.options.clone().unwrap_or_default(),
    });

    let only_when = d.only_when.as_ref().map(|o| proto::OnlyWhen {
        field: o.field.clone(),
        values: o.values.clone(),
    });

    proto::ParamDescriptor {
        key: d.key.clone(),
        label: d.label.clone(),
        description: d.description.clone(),
        only_when,
        mutable: d.mutable,
        value_type,
        constraints,
    }
}

#[cfg(feature = "component-server")]
pub(crate) fn map_current_values_to_metadata(
    map: &serde_json::Map<String, serde_json::Value>,
) -> std::collections::HashMap<String, String> {
    map.iter()
        .map(|(k, v)| {
            let val_str = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), val_str)
        })
        .collect()
}

// ── Client: MutableParamStore proxy ─────────────────────────

/// Implements [`MutableParamStore`] by forwarding each call over gRPC to a
/// remote `ParamService`. The orchestrator registers one of these per
/// remote component so its local [`ParamRegistry`] can dispatch `PATCH
/// /debug/params` updates uniformly.
pub struct RemoteParamStore {
    client: ParamServiceClient<super::auth::AuthedChannel>,
    component_id: String,
    endpoint: String,
}

impl std::fmt::Debug for RemoteParamStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteParamStore")
            .field("component_id", &self.component_id)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl RemoteParamStore {
    /// Connect to a remote `ParamService`.
    pub async fn connect(endpoint: &str, component_id: impl Into<String>) -> anyhow::Result<Self> {
        let channel = Channel::from_shared(endpoint.to_string())?
            .connect()
            .await
            .map_err(|e| anyhow::anyhow!("ParamService connect failed: {e}"))?;
        Ok(Self {
            client: ParamServiceClient::new(super::auth::authed_channel(channel)),
            component_id: component_id.into(),
            endpoint: endpoint.to_string(),
        })
    }

    /// Build from an already-connected channel (cheap clone -- preferred
    /// when reusing the same connection across multiple components).
    pub fn from_channel(
        channel: Channel,
        endpoint: impl Into<String>,
        component_id: impl Into<String>,
    ) -> Self {
        Self {
            client: ParamServiceClient::new(super::auth::authed_channel(channel)),
            component_id: component_id.into(),
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl MutableParamStore for RemoteParamStore {
    async fn set_param(&self, key: &str, value: ParamValue) -> Result<ParamValue, ParamError> {
        let mut client = self.client.clone();
        let req = proto::UpdateParamsRequest {
            component_id: self.component_id.clone(),
            key: key.to_string(),
            value: Some(param_value_to_proto(&value)),
        };
        // A hung component server must not stall the debug API
        // (and whatever holds the registry lock upstream) indefinitely.
        let resp =
            tokio::time::timeout(std::time::Duration::from_secs(5), client.update_params(req))
                .await
                .map_err(|_elapsed| {
                    ParamError::Remote("update_params timed out after 5s".to_string())
                })?
                .map_err(|e| ParamError::Remote(e.to_string()))?
                .into_inner();
        let applied = resp
            .applied
            .as_ref()
            .ok_or_else(|| ParamError::Remote("response missing 'applied'".to_string()))?;
        // Convert proto -> domain. Propagate type errors as Remote.
        match applied.kind.as_ref() {
            Some(proto::param_value::Kind::FloatVal(f)) => Ok(ParamValue::Float(*f)),
            Some(proto::param_value::Kind::IntVal(i)) => Ok(ParamValue::Int(*i)),
            Some(proto::param_value::Kind::BoolVal(b)) => Ok(ParamValue::Bool(*b)),
            Some(proto::param_value::Kind::StrVal(s)) => Ok(ParamValue::Str(s.clone())),
            None => Err(ParamError::Remote("applied.kind missing".to_string())),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
