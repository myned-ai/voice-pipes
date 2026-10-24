//! Generic per-component runtime parameter store and registry.
//!
//! This module provides the **infrastructure** for Layer 2 of the parameter
//! editor: applying validated mutable parameters to running components.
//!
//! Three pieces, no component-specific code:
//!
//! 1. [`MutableParamStore`] -- trait implemented by anything that holds
//!    mutable parameters (in-process components, gRPC client wrappers).
//!
//! 2. [`AtomicParamStore`] -- default lock-free implementation backed by
//!    [`std::sync::atomic`] primitives and [`arc_swap::ArcSwap`] for strings.
//!    Components that just need a fixed set of mutable fields use this
//!    directly via [`AtomicParamStoreBuilder`].
//!
//! 3. [`ParamRegistry`] -- process-wide map from component id to
//!    `(schema, store)`. The HTTP `PATCH /debug/params` handler is two
//!    lines and contains zero per-component branching.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::api::param_schema::{ParamDescriptor, ParamError, ParamValue};

// The store types live in the SDK so plugin providers can take live
// parameters too; the worker-wide registry is the runtime's.

pub use voice_pipes_sdk::params::{AtomicParamStore, AtomicParamStoreBuilder, MutableParamStore};

// ─────────────────────────────────────────────────────────────────────────────
// ParamRegistry -- process-wide dispatch map
// ─────────────────────────────────────────────────────────────────────────────

/// One registered component: its declared schema plus the store that backs it.
#[derive(Debug, Clone)]
pub struct RegisteredComponent {
    pub schema: Vec<ParamDescriptor>,
    pub store: Arc<dyn MutableParamStore>,
}

/// Worker-wide map from `PipelineNode.id` to its registered component.
///
/// The same `ParamRegistry` type is built inside `bin/component_server.rs`
/// for the gRPC `UpdateParams` RPC, so the dispatch logic lives in exactly
/// one place regardless of process boundaries.
#[derive(Debug, Default)]
pub struct ParamRegistry {
    components: RwLock<HashMap<String, RegisteredComponent>>,
}

impl ParamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a component by id.
    pub fn register(
        &self,
        id: impl Into<String>,
        schema: Vec<ParamDescriptor>,
        store: Arc<dyn MutableParamStore>,
    ) {
        let mut guard = self.components.write();
        guard.insert(id.into(), RegisteredComponent { schema, store });
    }

    /// Remove a component (used at shutdown / pipeline teardown).
    pub fn unregister(&self, id: &str) {
        let mut guard = self.components.write();
        guard.remove(id);
    }

    /// Return whether a component is currently registered.
    pub fn contains(&self, id: &str) -> bool {
        self.components.read().contains_key(id)
    }

    /// Look up the raw store for a component. Callers that need typed
    /// access (e.g. fetching atomic cells from an [`AtomicParamStore`])
    /// downcast via [`MutableParamStore::as_any`].
    pub fn store(&self, id: &str) -> Option<Arc<dyn MutableParamStore>> {
        self.components.read().get(id).map(|c| Arc::clone(&c.store))
    }

    /// Validate and apply a value. The single source of validation truth is
    /// [`ParamDescriptor::validate`], so the HTTP handler does not need to
    /// duplicate range/options checks.
    pub async fn apply(
        &self,
        component_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<ParamValue, ParamError> {
        let (descriptor, store) = {
            let guard = self.components.read();
            let comp = guard
                .get(component_id)
                .ok_or_else(|| ParamError::UnknownComponent(component_id.to_owned()))?;
            let descriptor = comp
                .schema
                .iter()
                .find(|d| d.key == key)
                .ok_or_else(|| ParamError::UnknownKey(key.to_owned()))?
                .clone();
            (descriptor, Arc::clone(&comp.store))
        };
        let typed = descriptor.validate(value)?;
        store.set_param(key, typed).await
    }

    /// Read the current value of a parameter, if the store supports readback.
    pub async fn get(&self, component_id: &str, key: &str) -> Option<ParamValue> {
        let store = {
            let guard = self.components.read();
            Arc::clone(&guard.get(component_id)?.store)
        };
        store.get_param(key).await
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/param_store.rs"]
mod tests;
