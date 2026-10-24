//! Parameter schema types for the graph-editor UI.
//!
//! [`SttFactory::provider_schema`](super::stt::SttFactory::provider_schema) returns a
//! [`ProviderSchema`] so the admin UI knows what fields to display and how to
//! validate live parameter updates.  Plugin authors fill this in via
//! [`ProviderSchema::push`] and the [`ParamDescriptor`] constructors.

use serde::Serialize;

// ── Descriptor building blocks ────────────────────────────────

/// Conditional visibility: show this param only when a named sibling field
/// has one of the listed values.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct OnlyWhen {
    /// Name of the sibling field this visibility rule depends on.
    pub field: String,
    /// Values of `field` for which this parameter is shown.
    pub values: Vec<String>,
}

impl OnlyWhen {
    /// Shown while `field` is one of `values`.
    pub fn new(field: impl Into<String>, values: Vec<String>) -> Self {
        Self {
            field: field.into(),
            values,
        }
    }
}

/// How to interpret and edit the value of a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ParamValueType {
    /// Floating-point number.
    Float,
    /// Integer.
    Int,
    /// Boolean toggle.
    Bool,
    /// Free-form string.
    Str,
    /// Filesystem path (rendered with a path picker).
    Path,
    /// One of a fixed set of string options (see [`ParamConstraints::options`]).
    Enum,
}

/// Allowed value range or enumeration for a parameter.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ParamConstraints {
    /// Inclusive lower bound for numeric types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Inclusive upper bound for numeric types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// For `Enum` type: exhaustive list of allowed string values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// For string types: longest accepted value in bytes. A runtime patch
    /// is bounded only by the request body limit otherwise, and a
    /// component that feeds the string into every turn (a prompt) would
    /// carry it whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<usize>,
}

impl ParamConstraints {
    with_setters! {
        with_min => min: Option<f64>,
        with_max => max: Option<f64>,
        with_options => options: Option<Vec<String>>,
        with_max_len => max_len: Option<usize>,
    }
}

/// Bound applied to [`ParamDescriptor::mutable_str`] descriptors that do
/// not set their own.
pub const DEFAULT_MAX_STR_LEN: usize = 4096;

/// Full descriptor for one configuration parameter of a pipeline component.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ParamDescriptor {
    /// Config key this descriptor controls (TOML field / options key).
    pub key: String,
    /// Short human-readable label for the UI.
    pub label: String,
    /// Longer help text shown next to the control.
    pub description: String,
    /// Conditional visibility rule, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only_when: Option<OnlyWhen>,
    /// `true` → can be changed at runtime; `false` → requires restart.
    pub mutable: bool,
    /// How the value is typed and edited.
    pub value_type: ParamValueType,
    /// Range / options restriction, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraints: Option<ParamConstraints>,
}

impl ParamDescriptor {
    /// A restart-to-change parameter `key` of `value_type`, shown always
    /// and unconstrained; the `with_*` setters change the rest. The typed
    /// constructors below cover the common cases.
    pub fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        value_type: ParamValueType,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            description: description.into(),
            only_when: None,
            mutable: false,
            value_type,
            constraints: None,
        }
    }

    with_setters! {
        with_only_when => only_when: Option<OnlyWhen>,
        with_mutable => mutable: bool,
        with_constraints => constraints: Option<ParamConstraints>,
    }

    /// Immutable string parameter (display + restart-to-change).
    pub fn static_str(key: &str, label: &str, description: &str) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            description: description.into(),
            only_when: None,
            mutable: false,
            value_type: ParamValueType::Str,
            constraints: None,
        }
    }

    /// Immutable enum parameter with a fixed option list.
    pub fn static_enum(key: &str, label: &str, description: &str, options: &[&str]) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            description: description.into(),
            only_when: None,
            mutable: false,
            value_type: ParamValueType::Enum,
            constraints: Some(ParamConstraints {
                min: None,
                max: None,
                options: Some(
                    options
                        .iter()
                        .map(std::string::ToString::to_string)
                        .collect(),
                ),
                max_len: None,
            }),
        }
    }

    /// Immutable filesystem-path parameter.
    pub fn static_path(key: &str, label: &str, description: &str) -> Self {
        Self {
            value_type: ParamValueType::Path,
            ..Self::static_str(key, label, description)
        }
    }

    /// Immutable boolean parameter.
    pub fn static_bool(key: &str, label: &str, description: &str) -> Self {
        Self {
            value_type: ParamValueType::Bool,
            ..Self::static_str(key, label, description)
        }
    }

    /// Runtime-tunable float parameter with an inclusive `[min, max]` range.
    pub fn mutable_float(key: &str, label: &str, description: &str, min: f64, max: f64) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            description: description.into(),
            only_when: None,
            mutable: true,
            value_type: ParamValueType::Float,
            constraints: Some(ParamConstraints {
                min: Some(min),
                max: Some(max),
                options: None,
                max_len: None,
            }),
        }
    }

    /// Runtime-tunable integer parameter with an inclusive `[min, max]` range.
    pub fn mutable_int(key: &str, label: &str, description: &str, min: i64, max: i64) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            description: description.into(),
            only_when: None,
            mutable: true,
            value_type: ParamValueType::Int,
            constraints: Some(ParamConstraints {
                min: Some(min as f64),
                max: Some(max as f64),
                options: None,
                max_len: None,
            }),
        }
    }

    /// Runtime-tunable boolean parameter.
    pub fn mutable_bool(key: &str, label: &str, description: &str) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            description: description.into(),
            only_when: None,
            mutable: true,
            value_type: ParamValueType::Bool,
            constraints: None,
        }
    }

    /// Runtime-tunable string parameter, bounded at
    /// [`DEFAULT_MAX_STR_LEN`] bytes; set `constraints.max_len` for a
    /// different bound.
    pub fn mutable_str(key: &str, label: &str, description: &str) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            description: description.into(),
            only_when: None,
            mutable: true,
            value_type: ParamValueType::Str,
            constraints: Some(ParamConstraints {
                min: None,
                max: None,
                options: None,
                max_len: Some(DEFAULT_MAX_STR_LEN),
            }),
        }
    }

    /// Immutable integer parameter (display-only, no range).
    pub fn static_int_display(key: &str, label: &str, description: &str) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            description: description.into(),
            only_when: None,
            mutable: false,
            value_type: ParamValueType::Int,
            constraints: None,
        }
    }

    /// Builder: shown with its range but changed only by a restart, for a
    /// setting read once when the component is built (no parameter store
    /// behind it).
    #[must_use]
    pub fn restart_required(mut self) -> Self {
        self.mutable = false;
        self
    }

    /// Builder: restrict visibility to when `field` equals any of `values`.
    #[must_use]
    pub fn only_when(mut self, field: &str, values: &[&str]) -> Self {
        self.only_when = Some(OnlyWhen {
            field: field.into(),
            values: values
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        });
        self
    }

    /// Validate a candidate value against this descriptor's type and constraints.
    pub fn validate(&self, value: serde_json::Value) -> Result<ParamValue, ParamError> {
        if !self.mutable {
            return Err(ParamError::Immutable);
        }
        let typed = ParamValue::from_json(value, self.value_type)?;
        if let Some(c) = &self.constraints {
            if matches!(typed, ParamValue::Float(_) | ParamValue::Int(_)) {
                let n = typed.as_f64()?;
                if c.min.is_some_and(|m| n < m) || c.max.is_some_and(|m| n > m) {
                    return Err(ParamError::OutOfRange {
                        value: n,
                        min: c.min,
                        max: c.max,
                    });
                }
            }
            if let (Ok(s), Some(opts)) = (typed.as_str(), c.options.as_ref())
                && !opts.iter().any(|o| o == s)
            {
                return Err(ParamError::NotInOptions(opts.clone()));
            }
            if let (Ok(s), Some(max_len)) = (typed.as_str(), c.max_len)
                && s.len() > max_len
            {
                return Err(ParamError::TooLong {
                    len: s.len(),
                    max_len,
                });
            }
        }
        Ok(typed)
    }
}

// ── Provider schema ───────────────────────────────────────────

/// The complete schema slice returned by [`super::stt::SttFactory::provider_schema`].
///
/// Contains ordered descriptors plus the current JSON values for each declared key.
/// Secrets (API keys, tokens) MUST NOT appear in `current_values` — emit a `*_set`
/// boolean placeholder instead.
#[derive(Debug, Default, Clone)]
pub struct ProviderSchema {
    /// Ordered parameter descriptors, as the UI should render them.
    pub descriptors: Vec<ParamDescriptor>,
    /// Current JSON value per declared key (never secrets -- see type docs).
    pub current_values: serde_json::Map<String, serde_json::Value>,
}

impl ProviderSchema {
    /// Minimal fallback schema that shows at least the provider name.
    /// Used as the default [`super::stt::SttFactory::provider_schema`] implementation.
    pub fn fallback(provider: &str, description: &str) -> Self {
        let mut current = serde_json::Map::new();
        current.insert("provider".into(), serde_json::json!(provider));
        Self {
            descriptors: vec![ParamDescriptor::static_str(
                "provider",
                "Provider",
                description,
            )],
            current_values: current,
        }
    }

    /// Append a descriptor and its current value in one call.
    pub fn push(
        &mut self,
        descriptor: ParamDescriptor,
        value: impl Into<serde_json::Value>,
    ) -> &mut Self {
        self.current_values
            .insert(descriptor.key.clone(), value.into());
        self.descriptors.push(descriptor);
        self
    }

    /// Append a descriptor whose current value is `null` (field is unset / optional).
    pub fn push_null(&mut self, descriptor: ParamDescriptor) -> &mut Self {
        self.current_values
            .insert(descriptor.key.clone(), serde_json::Value::Null);
        self.descriptors.push(descriptor);
        self
    }

    /// Pass-through remaining `options` map entries, skipping any key
    /// already listed in `skip_keys` and any credential-shaped key (see
    /// [`crate::secrets::is_secret_key`]). Values that pass are still
    /// scrubbed of embedded URL credentials.
    pub fn passthrough_options(
        &mut self,
        opts: &std::collections::HashMap<String, serde_json::Value>,
        skip_keys: &[&str],
        description: &str,
    ) -> &mut Self {
        for (k, v) in opts {
            if skip_keys.contains(&k.as_str()) || crate::secrets::is_secret_key(k) {
                continue;
            }
            self.descriptors
                .push(ParamDescriptor::static_str(k, k, description));
            self.current_values
                .insert(k.clone(), crate::secrets::redacted(v, "***"));
        }
        self
    }
}

// ── Typed parameter values (for validate()) ───────────────────

/// Strongly-typed parameter value, returned by [`ParamDescriptor::validate`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ParamValue {
    /// Floating-point value.
    Float(f64),
    /// Integer value.
    Int(i64),
    /// Boolean value.
    Bool(bool),
    /// String value (also used for `Path` and `Enum` types).
    Str(String),
}

/// Error returned by [`ParamDescriptor::validate`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ParamError {
    /// No component with this id is registered.
    #[error("unknown component: {0}")]
    UnknownComponent(String),
    /// The component has no parameter with this key.
    #[error("unknown parameter key: {0}")]
    UnknownKey(String),
    /// The parameter is not runtime-mutable.
    #[error("parameter is immutable, requires worker restart")]
    Immutable,
    /// The supplied value has the wrong JSON type.
    #[error("type mismatch: expected {expected:?}, got {got}")]
    TypeMismatch {
        /// The type the descriptor declares.
        expected: ParamValueType,
        /// Human-readable kind of the value actually supplied.
        got: &'static str,
    },
    /// The numeric value violates the descriptor's constraints.
    #[error("value {value} out of range [{min:?}, {max:?}]")]
    OutOfRange {
        /// The rejected value.
        value: f64,
        /// Inclusive lower bound, if declared.
        min: Option<f64>,
        /// Inclusive upper bound, if declared.
        max: Option<f64>,
    },
    /// The string value is not one of the enum's allowed options.
    #[error("value not in allowed options: {0:?}")]
    NotInOptions(Vec<String>),
    /// A string value longer than the descriptor allows.
    #[error("value is {len} bytes, longer than the {max_len} byte limit")]
    TooLong {
        /// Length of the rejected value in bytes.
        len: usize,
        /// The declared bound.
        max_len: usize,
    },
    /// A remote (gRPC-hosted) component rejected the update.
    #[error("remote component error: {0}")]
    Remote(String),
}

impl ParamValue {
    /// Coerce a JSON value into a typed [`ParamValue`] according to `ty`.
    ///
    /// Whole-number floats coerce to `Int` when an integer is expected.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public SDK API; the by-value signature is part of the stable interface"
    )]
    pub fn from_json(v: serde_json::Value, ty: ParamValueType) -> Result<Self, ParamError> {
        let got = json_kind(&v);
        match ty {
            ParamValueType::Float => v
                .as_f64()
                .map(ParamValue::Float)
                .ok_or(ParamError::TypeMismatch { expected: ty, got }),
            ParamValueType::Int => {
                if let Some(i) = v.as_i64() {
                    Ok(ParamValue::Int(i))
                } else if let Some(f) = v.as_f64() {
                    if f.fract() == 0.0 && f.is_finite() {
                        Ok(ParamValue::Int(f as i64))
                    } else {
                        Err(ParamError::TypeMismatch {
                            expected: ty,
                            got: "non-integer number",
                        })
                    }
                } else {
                    Err(ParamError::TypeMismatch { expected: ty, got })
                }
            }
            ParamValueType::Bool => v
                .as_bool()
                .map(ParamValue::Bool)
                .ok_or(ParamError::TypeMismatch { expected: ty, got }),
            ParamValueType::Str | ParamValueType::Path | ParamValueType::Enum => v
                .as_str()
                .map(|s| ParamValue::Str(s.to_owned()))
                .ok_or(ParamError::TypeMismatch { expected: ty, got }),
        }
    }

    /// Convert back into a JSON value.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            ParamValue::Float(f) => serde_json::json!(f),
            ParamValue::Int(i) => serde_json::json!(i),
            ParamValue::Bool(b) => serde_json::json!(b),
            ParamValue::Str(s) => serde_json::json!(s),
        }
    }

    /// Numeric view: `Float` as-is, `Int` widened; other kinds error.
    pub fn as_f64(&self) -> Result<f64, ParamError> {
        match self {
            ParamValue::Float(f) => Ok(*f),
            ParamValue::Int(i) => Ok(*i as f64),
            other => Err(ParamError::TypeMismatch {
                expected: ParamValueType::Float,
                got: other.kind(),
            }),
        }
    }

    /// Integer view: `Int` as-is, whole-number `Float`s coerce; others error.
    pub fn as_i64(&self) -> Result<i64, ParamError> {
        match self {
            ParamValue::Int(i) => Ok(*i),
            ParamValue::Float(f) if f.fract() == 0.0 && f.is_finite() => Ok(*f as i64),
            other => Err(ParamError::TypeMismatch {
                expected: ParamValueType::Int,
                got: other.kind(),
            }),
        }
    }

    /// Boolean view; non-`Bool` kinds error.
    pub fn as_bool(&self) -> Result<bool, ParamError> {
        match self {
            ParamValue::Bool(b) => Ok(*b),
            other => Err(ParamError::TypeMismatch {
                expected: ParamValueType::Bool,
                got: other.kind(),
            }),
        }
    }

    /// String view; non-`Str` kinds error.
    pub fn as_str(&self) -> Result<&str, ParamError> {
        match self {
            ParamValue::Str(s) => Ok(s.as_str()),
            other => Err(ParamError::TypeMismatch {
                expected: ParamValueType::Str,
                got: other.kind(),
            }),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            ParamValue::Float(_) => "float",
            ParamValue::Int(_) => "int",
            ParamValue::Bool(_) => "bool",
            ParamValue::Str(_) => "string",
        }
    }
}

fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
#[path = "tests/schema.rs"]
mod tests;
