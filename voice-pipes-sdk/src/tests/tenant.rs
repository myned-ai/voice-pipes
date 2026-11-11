use std::str::FromStr;

use crate::tenant::{TenantId, TenantIdError, TenantRetentionPolicy, MAX_TENANT_ID_LEN};

#[test]
fn valid_tenant_ids_are_accepted() {
    let ids = ["default", "tenant-1", "org_abc_123", "AcmeCorp-Production", "a"];
    for id in ids {
        let tenant = TenantId::new(id).expect("valid tenant id");
        assert_eq!(tenant.as_str(), id);
        assert_eq!(&*tenant, id);
        assert_eq!(tenant.to_string(), id);
    }
}

#[test]
fn empty_or_whitespace_is_rejected() {
    assert_eq!(TenantId::new("").unwrap_err(), TenantIdError::Empty);
    assert_eq!(TenantId::new("   ").unwrap_err(), TenantIdError::Empty);
}

#[test]
fn too_long_is_rejected() {
    let long_id = "a".repeat(MAX_TENANT_ID_LEN + 1);
    match TenantId::new(&long_id) {
        Err(TenantIdError::TooLong { max, actual }) => {
            assert_eq!(max, MAX_TENANT_ID_LEN);
            assert_eq!(actual, MAX_TENANT_ID_LEN + 1);
        }
        other => panic!("expected TooLong, got {other:?}"),
    }
}

#[test]
fn path_traversal_and_invalid_chars_are_rejected() {
    let invalid = [
        "../traversal",
        "/absolute/path",
        "tenant/sub",
        "tenant\\windows",
        "-starts-with-hyphen",
        "_starts-with-underscore",
        "has spaces",
        "tenant@domain",
        "tenant.dot",
        "tenant:colon",
    ];
    for bad in invalid {
        match TenantId::from_str(bad) {
            Err(TenantIdError::InvalidCharacters(s)) => {
                assert_eq!(s, bad);
            }
            other => panic!("expected InvalidCharacters for '{bad}', got {other:?}"),
        }
    }
}

#[test]
fn json_serialization_and_deserialization_round_trips() {
    let tenant = TenantId::new("tenant-abc").unwrap();
    let json = serde_json::to_string(&tenant).unwrap();
    assert_eq!(json, "\"tenant-abc\"");

    let deserialized: TenantId = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, tenant);

    let invalid_json = "\"../bad-tenant\"";
    let err = serde_json::from_str::<TenantId>(invalid_json);
    assert!(err.is_err());
}

#[test]
fn default_retention_policy_values() {
    let policy = TenantRetentionPolicy::default();
    assert_eq!(policy.retention_days, 7);
    assert_eq!(policy.max_session_bytes, 512 * 1024 * 1024);
    assert!(!policy.zero_data_retention);
}
