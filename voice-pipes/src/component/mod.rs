//! Common component base -- health, telemetry, and error policy.
//!
//! Every pipeline stage (STT, TTS, text processor, VAD, transport) implements
//! the [`Component`] trait so the orchestrator can ask any stage uniformly:
//! *Are you healthy?*, *What are your rolling metrics?*, *What should I do
//! when you fail?*
//!
//! The trait and its data model are the SDK's, so a plugin's component and a
//! built-in one are the same type. The engine adds what only it needs:
//! - [`error_policy`]: [`ErrorAction`] and [`classify_streaming_error`], the
//!   orchestrator's reading of a component's [`ErrorPolicy`].
//! - [`registry`]: the process-wide [`ComponentRegistry`].
//! - [`auto_degrade`]: the periodic [`AutoDegrader`] task.

mod auto_degrade;
mod error_policy;
mod registry;

#[cfg(test)]
mod test_support;

pub use auto_degrade::{AutoDegradeConfig, AutoDegrader};
pub use error_policy::{ErrorAction, classify_streaming_error};
pub use registry::ComponentRegistry;
pub use voice_pipes_sdk::component::{
    Component, ComponentCore, ComponentKind, ComponentTelemetry, CoreComponent, DeviceInfo,
    ErrorPolicy, HealthState, HealthStatus, ModelIdentity,
};
