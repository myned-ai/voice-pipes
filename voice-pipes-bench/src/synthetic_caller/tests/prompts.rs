use super::*;

#[test]
fn lookup_resolves_known_templates() {
    assert_eq!(lookup("polite_time"), Some(POLITE_TIME));
    assert_eq!(lookup("impatient_refund"), Some(IMPATIENT_REFUND));
    assert_eq!(lookup("curious_greeter"), Some(CURIOUS_GREETER));
    assert_eq!(lookup("barge_in_tester"), Some(BARGE_IN_TESTER));
}

#[test]
fn lookup_is_case_insensitive() {
    assert_eq!(lookup("POLITE_TIME"), Some(POLITE_TIME));
    assert_eq!(lookup("Polite_Time"), Some(POLITE_TIME));
}

#[test]
fn lookup_returns_none_for_unknown() {
    assert!(lookup("nonexistent_persona").is_none());
    assert!(lookup("").is_none());
}

#[test]
fn all_templates_are_terse() {
    // Personas should be short to keep cost bounded; cap at 600 chars.
    for name in TEMPLATE_NAMES {
        let p = lookup(name).unwrap();
        assert!(
            p.len() < 600,
            "persona {name} is too long: {} chars",
            p.len()
        );
        assert!(!p.is_empty(), "persona {name} is empty");
    }
}

#[test]
fn template_names_round_trip_through_lookup() {
    for name in TEMPLATE_NAMES {
        assert!(lookup(name).is_some(), "{name} missing from lookup()");
    }
}
