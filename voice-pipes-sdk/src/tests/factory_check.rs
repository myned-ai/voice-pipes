use super::*;

/// A config is a list of the fields wrong in it.
type Cfg = Vec<&'static str>;

fn validate(cfg: &Cfg) -> Vec<ConfigError> {
    cfg.iter().map(|f| ConfigError::new(*f, "wrong")).collect()
}

/// A build that refuses exactly what validation does.
fn agrees(cfg: &Cfg) -> Result<(), BuildError> {
    let errors = validate(cfg);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(BuildError::Config(errors))
    }
}

#[test]
fn a_build_that_refuses_as_validate_does_passes() {
    build_refuses_what_validate_refuses(&[vec!["a"], vec!["a", "b"]], validate, agrees);
}

#[test]
#[should_panic(expected = "build accepts")]
fn a_build_that_accepts_a_refused_config_fails() {
    build_refuses_what_validate_refuses(&[vec!["a"]], validate, |_| Ok::<(), BuildError>(()));
}

#[test]
#[should_panic(expected = "not as a config refusal")]
fn a_build_that_fails_some_other_way_fails() {
    build_refuses_what_validate_refuses(&[vec!["a"]], validate, |_| {
        Err::<(), _>(BuildError::failed("model not found"))
    });
}

#[test]
#[should_panic(expected = "does not report b")]
fn a_build_that_reports_less_than_validate_fails() {
    build_refuses_what_validate_refuses(&[vec!["a", "b"]], validate, |_| {
        Err::<(), _>(BuildError::Config(vec![ConfigError::new("a", "wrong")]))
    });
}

#[test]
#[should_panic(expected = "validate accepts")]
fn a_config_validate_accepts_is_a_mistake_in_the_test() {
    build_refuses_what_validate_refuses(&[vec![]], validate, agrees);
}
