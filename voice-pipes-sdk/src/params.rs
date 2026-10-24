//! Parameters an operator can change while the pipeline runs.
//!
//! A provider declares which of its settings are live in its
//! `provider_schema` (a `ParamDescriptor::mutable_*`). The framework builds an
//! [`AtomicParamStore`] with those fields and hands it to the factory's
//! `build` as [`BuildContext::params`](crate::BuildContext::params); the
//! provider keeps the cells it cares about (a voice, a
//! system prompt, a temperature) and reads them on each request, so a change
//! from the admin console (`PATCH /debug/params`) takes effect on the next
//! synthesis or turn without a restart.
//!
//! ```
//! use voice_pipes_sdk::params::AtomicParamStore;
//!
//! let store = AtomicParamStore::builder().string("voice", "alba").build();
//! let voice = store.string_cell("voice").expect("declared");
//! assert_eq!(**voice.load(), "alba");
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use arc_swap::ArcSwap;
use async_trait::async_trait;

use crate::schema::{ParamError, ParamValue, ParamValueType, ProviderSchema};

// ─────────────────────────────────────────────────────────────────────────────
// MutableParamStore trait
// ─────────────────────────────────────────────────────────────────────────────

/// Implemented by any component that exposes runtime-mutable parameters.
///
/// In-process components write to local atomics; gRPC client wrappers forward
/// the call to the remote component server. The HTTP handler does not branch
/// on which kind it is dispatching to.
#[async_trait]
pub trait MutableParamStore: Send + Sync + std::fmt::Debug {
    /// Apply a validated value. Returns the value that ended up being stored
    /// (may differ from the input if e.g. the remote normalised it). The
    /// handler echoes this back to the frontend so it can refresh.
    async fn set_param(&self, key: &str, value: ParamValue) -> Result<ParamValue, ParamError>;

    /// Read the current value, if available.
    ///
    /// Default returns `None`; in-process atomic stores override this so
    /// callers can confirm or refresh without an extra round-trip.
    async fn get_param(&self, _key: &str) -> Option<ParamValue> {
        None
    }

    /// Concrete-type access for callers that need to fetch atomic cells
    /// (e.g. session setup adopting cells from an [`AtomicParamStore`]).
    /// Implementations typically just `self` here.
    fn as_any(&self) -> &dyn std::any::Any;
}

// ─────────────────────────────────────────────────────────────────────────────
// AtomicParamStore -- generic lock-free store for the common case
// ─────────────────────────────────────────────────────────────────────────────

/// Lock-free atomic-backed parameter store.
///
/// Components that just need a fixed set of mutable f64/i64/bool/string
/// fields use this directly. Reads on the hot path are a single atomic load
/// (or an `ArcSwap` snapshot for strings).
#[derive(Debug, Default)]
pub struct AtomicParamStore {
    floats: HashMap<String, Arc<AtomicU64>>, // f64 stored as bits
    ints: HashMap<String, Arc<AtomicI64>>,
    bools: HashMap<String, Arc<AtomicBool>>,
    strings: HashMap<String, Arc<ArcSwap<String>>>,
}

impl AtomicParamStore {
    /// Start declaring the store's fields.
    pub fn builder() -> AtomicParamStoreBuilder {
        AtomicParamStoreBuilder::default()
    }

    /// The store a provider's schema describes: one cell per live
    /// (`mutable`) descriptor, holding the schema's current value. This is
    /// how the framework builds a provider's store, so a provider declares
    /// its live fields once, in `provider_schema`, and nowhere else.
    ///
    /// A descriptor whose current value is missing gets no cell, except a
    /// float or string reported as `null`: floats get `NaN` (the "unset"
    /// sentinel sampling parameters use) and strings the empty string. A
    /// string given as an array of strings (a list of stop sequences) is
    /// stored newline-joined.
    pub fn from_schema(schema: &ProviderSchema) -> Self {
        use serde_json::Value;
        let mut b = Self::builder();
        for d in schema.descriptors.iter().filter(|d| d.mutable) {
            let Some(value) = schema.current_values.get(&d.key) else {
                continue;
            };
            b = match (d.value_type, value) {
                (ParamValueType::Float, Value::Null) => b.float(&d.key, f64::NAN),
                (ParamValueType::Float, v) => match v.as_f64() {
                    Some(f) => b.float(&d.key, f),
                    None => b,
                },
                (ParamValueType::Int, v) => match v.as_i64() {
                    Some(i) => b.int(&d.key, i),
                    None => b,
                },
                (ParamValueType::Bool, v) => match v.as_bool() {
                    Some(x) => b.bool(&d.key, x),
                    None => b,
                },
                (_, Value::Null) => b.string(&d.key, String::new()),
                (_, Value::String(s)) => b.string(&d.key, s.clone()),
                (_, Value::Array(items)) => b.string(
                    &d.key,
                    items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                (_, other) => b.string(&d.key, other.to_string()),
            };
        }
        b.build()
    }

    /// Read the current value of a float-typed param.
    pub fn float(&self, key: &str) -> Option<f64> {
        self.floats
            .get(key)
            .map(|c| f64::from_bits(c.load(Ordering::Relaxed)))
    }

    /// Read the current value of an int-typed param.
    pub fn int(&self, key: &str) -> Option<i64> {
        self.ints.get(key).map(|c| c.load(Ordering::Relaxed))
    }

    /// Read the current value of a bool-typed param.
    pub fn bool(&self, key: &str) -> Option<bool> {
        self.bools.get(key).map(|c| c.load(Ordering::Relaxed))
    }

    /// Read the current value of a string-typed param. Returns an `Arc<String>`
    /// snapshot (lock-free, cheap clone).
    pub fn string(&self, key: &str) -> Option<Arc<String>> {
        self.strings.get(key).map(|c| c.load_full())
    }

    /// Borrow the underlying `Arc<AtomicU64>` cell so a hot-path consumer can
    /// hold its own clone and avoid the `HashMap` lookup on every read.
    pub fn float_cell(&self, key: &str) -> Option<Arc<AtomicU64>> {
        self.floats.get(key).cloned()
    }

    /// The shared cell behind an int param (see [`Self::float_cell`]).
    pub fn int_cell(&self, key: &str) -> Option<Arc<AtomicI64>> {
        self.ints.get(key).cloned()
    }

    /// The shared cell behind a bool param (see [`Self::float_cell`]).
    pub fn bool_cell(&self, key: &str) -> Option<Arc<AtomicBool>> {
        self.bools.get(key).cloned()
    }

    /// The shared cell behind a string param: hold it, and every read sees
    /// the value an operator last set.
    pub fn string_cell(&self, key: &str) -> Option<Arc<ArcSwap<String>>> {
        self.strings.get(key).cloned()
    }
}

#[async_trait]
impl MutableParamStore for AtomicParamStore {
    async fn set_param(&self, key: &str, value: ParamValue) -> Result<ParamValue, ParamError> {
        if let Some(cell) = self.floats.get(key) {
            let n = value.as_f64()?;
            cell.store(n.to_bits(), Ordering::Relaxed);
            return Ok(ParamValue::Float(n));
        }
        if let Some(cell) = self.ints.get(key) {
            let n = value.as_i64()?;
            cell.store(n, Ordering::Relaxed);
            return Ok(ParamValue::Int(n));
        }
        if let Some(cell) = self.bools.get(key) {
            let b = value.as_bool()?;
            cell.store(b, Ordering::Relaxed);
            return Ok(ParamValue::Bool(b));
        }
        if let Some(cell) = self.strings.get(key) {
            let s = value.as_str()?.to_owned();
            cell.store(Arc::new(s.clone()));
            return Ok(ParamValue::Str(s));
        }
        Err(ParamError::UnknownKey(key.to_owned()))
    }

    async fn get_param(&self, key: &str) -> Option<ParamValue> {
        if let Some(v) = self.float(key) {
            return Some(ParamValue::Float(v));
        }
        if let Some(v) = self.int(key) {
            return Some(ParamValue::Int(v));
        }
        if let Some(v) = self.bool(key) {
            return Some(ParamValue::Bool(v));
        }
        if let Some(v) = self.string(key) {
            return Some(ParamValue::Str((*v).clone()));
        }
        None
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Builder for [`AtomicParamStore`]. Each call declares one mutable field.
#[derive(Debug, Default)]
pub struct AtomicParamStoreBuilder {
    inner: AtomicParamStore,
}

impl AtomicParamStoreBuilder {
    /// Declare a float field with its starting value.
    #[must_use]
    pub fn float(mut self, key: &str, initial: f64) -> Self {
        self.inner
            .floats
            .insert(key.to_owned(), Arc::new(AtomicU64::new(initial.to_bits())));
        self
    }

    /// Declare an int field with its starting value.
    #[must_use]
    pub fn int(mut self, key: &str, initial: i64) -> Self {
        self.inner
            .ints
            .insert(key.to_owned(), Arc::new(AtomicI64::new(initial)));
        self
    }

    /// Declare a bool field with its starting value.
    #[must_use]
    pub fn bool(mut self, key: &str, initial: bool) -> Self {
        self.inner
            .bools
            .insert(key.to_owned(), Arc::new(AtomicBool::new(initial)));
        self
    }

    /// Declare a string field with its starting value.
    #[must_use]
    pub fn string(mut self, key: &str, initial: impl Into<String>) -> Self {
        self.inner.strings.insert(
            key.to_owned(),
            Arc::new(ArcSwap::from_pointee(initial.into())),
        );
        self
    }

    /// The finished store.
    pub fn build(self) -> AtomicParamStore {
        self.inner
    }
}

#[cfg(test)]
#[path = "tests/params.rs"]
mod tests;
