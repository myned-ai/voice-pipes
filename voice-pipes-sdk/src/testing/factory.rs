//! A factory's build refuses what its validate refuses.

use std::fmt::Debug;

use crate::{BuildError, ConfigError};

/// Check that `build` refuses every config `validate` refuses, as a config
/// refusal ([`BuildError::Config`]) naming every field `validate` named.
///
/// Validation runs when a config is checked (`voice-pipes check`, the
/// wizard, the console's save); building runs when a pipeline starts. A
/// factory whose build skips a check its validate makes starts a pipeline
/// on settings it would have refused, and one whose build fails on them
/// some other way (a model that is not there, a connection) reports the
/// wrong problem. Give it configs `validate` refuses and that need nothing
/// on disk or on the network to refuse: an option the factory does not
/// read, a value out of range, a wrong type.
///
/// # Panics
///
/// When `validate` accepts one of `configs` (the test passed the wrong
/// config), when `build` accepts it, when `build` fails other than as a
/// config refusal, or when its refusal leaves out a field `validate` named.
pub fn build_refuses_what_validate_refuses<C, T>(
    configs: &[C],
    validate: impl Fn(&C) -> Vec<ConfigError>,
    build: impl Fn(&C) -> Result<T, BuildError>,
) where
    C: Debug,
{
    for cfg in configs {
        let refused = validate(cfg);
        assert!(
            !refused.is_empty(),
            "validate accepts {cfg:?}; give this check configs it refuses"
        );
        let Err(err) = build(cfg) else {
            panic!("build accepts {cfg:?}, which validate refuses: {refused:?}")
        };
        let built: Vec<&str> = err
            .config_errors()
            .iter()
            .map(|e| e.field.as_str())
            .collect();
        assert!(
            !built.is_empty(),
            "build of {cfg:?} failed, but not as a config refusal: {err}"
        );
        for e in &refused {
            assert!(
                built.contains(&e.field.as_str()),
                "build of {cfg:?} does not report {}, which validate does: {err}",
                e.field
            );
        }
    }
}

#[cfg(test)]
#[path = "../tests/factory_check.rs"]
mod tests;
