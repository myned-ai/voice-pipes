//! Reading a component's `options` table.
//!
//! Every provider has a free-form option map beside its typed fields. A
//! [`ProviderOptions`] reads it the one way: each value typed and bounded,
//! each problem a [`ConfigError`] naming `<section>.<key>`, and every key
//! nothing read refused ([`ConfigErrorKind::UnknownKey`]), so a misspelt or
//! leftover setting is an error rather than a silent default. A reader
//! keeps going past a problem (the read returns the default), so one pass
//! reports all of them; parse a config with it once, and let `validate`
//! and `build` share that parse.
//!
//! ```
//! use std::collections::HashMap;
//! use voice_pipes_sdk::options::ProviderOptions;
//!
//! let options: HashMap<String, serde_json::Value> =
//!     [("speed".to_string(), serde_json::json!(9.0))].into();
//! let mut o = ProviderOptions::new("tts", &options);
//! let speed = o.f64_in("speed", 0.25..=4.0, 1.0);
//! let timeout_ms = o.u64_in("request_timeout_ms", 1..=600_000, 30_000);
//! assert_eq!((speed, timeout_ms), (1.0, 30_000));
//! let errors = o.finish().unwrap_err();
//! assert_eq!(errors[0].field, "tts.speed");
//! ```

use std::collections::{BTreeSet, HashMap};
use std::ops::RangeInclusive;

use crate::config::{ConfigError, ConfigErrorKind};

/// The option that lets a hosted provider send its key over plain HTTP to
/// a non-local endpoint. The endpoint check reads it, not the provider.
pub const ALLOW_INSECURE_OPTION: &str = "allow_insecure_endpoint";

/// The rate a speech-to-speech session emits the model's audio at. The
/// engine reads it, not the provider.
pub const OUTPUT_EMIT_RATE_OPTION: &str = "output_emit_rate";

/// The `[realtime_provider.options]` keys the engine's session runner reads
/// itself (the system prompt and the neutral turn-detection settings it
/// maps for every provider), beside [`OUTPUT_EMIT_RATE_OPTION`].
pub const REALTIME_ENGINE_OPTIONS: &[&str] = &[
    OUTPUT_EMIT_RATE_OPTION,
    "instructions",
    "vad_eagerness",
    "vad_prefix_padding_ms",
    "vad_silence_ms",
    "vad_threshold",
];

/// Reads one component's option map; see the [module docs](self).
#[derive(Debug)]
pub struct ProviderOptions<'a> {
    section: &'a str,
    options: &'a HashMap<String, serde_json::Value>,
    read: BTreeSet<&'a str>,
    errors: Vec<ConfigError>,
}

impl<'a> ProviderOptions<'a> {
    /// A reader over `options`, naming problems `<section>.<key>`.
    pub fn new(section: &'a str, options: &'a HashMap<String, serde_json::Value>) -> Self {
        Self {
            section,
            options,
            read: BTreeSet::new(),
            errors: Vec::new(),
        }
    }

    /// The same reader, with `keys` read by the engine rather than the
    /// component: present or not, they are not unknown.
    #[must_use]
    pub fn with_engine_keys(mut self, keys: &[&'a str]) -> Self {
        self.read.extend(keys.iter().copied());
        self
    }

    fn get(&mut self, key: &str) -> Option<&'a serde_json::Value> {
        let (stored, value) = self.options.get_key_value(key)?;
        self.read.insert(stored.as_str());
        Some(value)
    }

    fn field(&self, key: &str) -> String {
        format!("{}.{key}", self.section)
    }

    /// Record a problem with `key`, one the caller's own checks found.
    pub fn error(&mut self, key: &str, message: impl Into<String>) {
        let field = self.field(key);
        self.errors.push(ConfigError::new(field, message));
    }

    fn wrong_type(&mut self, key: &str, wanted: &str, got: &serde_json::Value) {
        self.error(key, format!("must be {wanted}, got {got}"));
    }

    /// The value at `key`, as it is (a table, a list): mark it read.
    pub fn value(&mut self, key: &str) -> Option<&'a serde_json::Value> {
        self.get(key)
    }

    /// A string, if set.
    pub fn string(&mut self, key: &str) -> Option<String> {
        match self.get(key)? {
            serde_json::Value::String(s) => Some(s.clone()),
            other => {
                self.wrong_type(key, "a string", other);
                None
            }
        }
    }

    /// A string, or `default`.
    pub fn string_or(&mut self, key: &str, default: &str) -> String {
        self.string(key).unwrap_or_else(|| default.to_string())
    }

    /// One of `allowed`, or `default` when unset.
    pub fn one_of(&mut self, key: &str, allowed: &[&str], default: &str) -> String {
        let Some(value) = self.string(key) else {
            return default.to_string();
        };
        if allowed.contains(&value.as_str()) {
            value
        } else {
            self.error(key, format!("must be one of {allowed:?}, got \"{value}\""));
            default.to_string()
        }
    }

    /// A boolean, if set.
    pub fn optional_bool(&mut self, key: &str) -> Option<bool> {
        match self.get(key)? {
            serde_json::Value::Bool(b) => Some(*b),
            other => {
                self.wrong_type(key, "true or false", other);
                None
            }
        }
    }

    /// A boolean, or `default`.
    pub fn bool(&mut self, key: &str, default: bool) -> bool {
        self.optional_bool(key).unwrap_or(default)
    }

    /// A list of strings, if set.
    pub fn string_list(&mut self, key: &str) -> Option<Vec<String>> {
        let value = self.get(key)?;
        let strings: Option<Vec<String>> = value.as_array().and_then(|items| {
            items
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect()
        });
        if strings.is_none() {
            self.wrong_type(key, "a list of strings", value);
        }
        strings
    }

    /// A whole number within `range`, if set.
    pub fn optional_u64_in(&mut self, key: &str, range: RangeInclusive<u64>) -> Option<u64> {
        let value = self.get(key)?;
        match value.as_u64() {
            Some(n) if range.contains(&n) => Some(n),
            Some(n) => {
                self.error(
                    key,
                    format!("must be from {} to {}, got {n}", range.start(), range.end()),
                );
                None
            }
            None => {
                let wanted = format!("a whole number from {} to {}", range.start(), range.end());
                self.wrong_type(key, &wanted, value);
                None
            }
        }
    }

    /// A whole number within `range`, or `default`.
    pub fn u64_in(&mut self, key: &str, range: RangeInclusive<u64>, default: u64) -> u64 {
        self.optional_u64_in(key, range).unwrap_or(default)
    }

    /// A whole number within `range` as a `u32`, if set.
    pub fn optional_u32_in(&mut self, key: &str, range: RangeInclusive<u32>) -> Option<u32> {
        let wide = u64::from(*range.start())..=u64::from(*range.end());
        self.optional_u64_in(key, wide)
            .and_then(|n| u32::try_from(n).ok())
    }

    /// A whole number within `range` as a `u32`, or `default`.
    pub fn u32_in(&mut self, key: &str, range: RangeInclusive<u32>, default: u32) -> u32 {
        self.optional_u32_in(key, range).unwrap_or(default)
    }

    /// A whole number within `range` as a `usize`, or `default`.
    pub fn usize_in(&mut self, key: &str, range: RangeInclusive<usize>, default: usize) -> usize {
        let wide = u64::try_from(*range.start()).unwrap_or(u64::MAX)
            ..=u64::try_from(*range.end()).unwrap_or(u64::MAX);
        self.optional_u64_in(key, wide)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(default)
    }

    /// A number within `range`, if set. A whole number reads as one.
    pub fn optional_f64_in(&mut self, key: &str, range: RangeInclusive<f64>) -> Option<f64> {
        let value = self.get(key)?;
        match value.as_f64() {
            Some(x) if range.contains(&x) => Some(x),
            Some(x) => {
                self.error(
                    key,
                    format!("must be from {} to {}, got {x}", range.start(), range.end()),
                );
                None
            }
            None => {
                let wanted = format!("a number from {} to {}", range.start(), range.end());
                self.wrong_type(key, &wanted, value);
                None
            }
        }
    }

    /// A number within `range`, or `default`.
    pub fn f64_in(&mut self, key: &str, range: RangeInclusive<f64>, default: f64) -> f64 {
        self.optional_f64_in(key, range).unwrap_or(default)
    }

    /// Every problem found, then one for each key nothing read, sorted by
    /// key. `Ok` when there are none.
    ///
    /// # Errors
    /// The problems, as [`ConfigError`]s.
    pub fn finish(self) -> Result<(), Vec<ConfigError>> {
        let errors = self.into_errors();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// The problems with the values read, leaving other keys alone: for a
    /// reader that checks some of a table the component shares.
    pub fn into_value_errors(self) -> Vec<ConfigError> {
        self.errors
    }

    /// [`finish`](Self::finish)'s problems, as a list (empty when none).
    pub fn into_errors(mut self) -> Vec<ConfigError> {
        let mut unknown: Vec<&String> = self
            .options
            .keys()
            .filter(|k| !self.read.contains(k.as_str()))
            .collect();
        unknown.sort();
        for key in unknown {
            let error = ConfigError::new(
                format!("{}.{key}", self.section),
                format!("not a setting of this {}", self.section),
            )
            .with_kind(ConfigErrorKind::UnknownKey);
            self.errors.push(error);
        }
        self.errors
    }
}

/// A parse's problems as the error `build` returns, each on its own line.
pub fn refused(errors: Vec<ConfigError>) -> crate::BuildError {
    crate::BuildError::Config(errors)
}

/// `Ok` when `errors` is empty, else the build's refusal of them: the first
/// line of a factory's `build`, given its own `validate`, so a build refuses
/// what validation refuses and says why the same way
/// ([`crate::testing::build_refuses_what_validate_refuses`] checks it).
///
/// # Errors
/// [`crate::BuildError::Config`] with `errors`, when there are any.
pub fn validated(errors: Vec<ConfigError>) -> Result<(), crate::BuildError> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(refused(errors))
    }
}

/// A setting's `f64`, as the readers return it, at the `f32` a provider
/// keeps or sends: precision is lost, not the value, and a value past
/// `f32`'s range becomes an infinity rather than a panic.
///
/// ```
/// use voice_pipes_sdk::options::as_f32;
///
/// assert_eq!(as_f32(0.75), 0.75_f32);
/// assert_eq!(as_f32(f64::MAX), f32::INFINITY);
/// ```
#[expect(
    clippy::cast_possible_truncation,
    reason = "an f64 to f32 cast rounds and saturates to infinity; it cannot panic"
)]
pub fn as_f32(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
#[path = "tests/options.rs"]
mod tests;
