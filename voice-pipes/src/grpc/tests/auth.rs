use super::*;

#[test]
fn server_check_passes_when_open() {
    let mut check = ServerBearerCheck::new(SurfaceAuth::open("test"));
    assert!(check.call(Request::new(())).is_ok());
}

#[test]
fn client_bearer_attaches_nothing_when_unset() {
    let mut bearer = ClientBearer { token: None };
    let req = bearer.call(Request::new(())).unwrap();
    assert!(req.metadata().get("authorization").is_none());
}

#[test]
fn client_bearer_roundtrips_through_server_check() {
    let mut bearer = ClientBearer {
        token: Some(Arc::from("tok-1")),
    };
    let req = bearer.call(Request::new(())).unwrap();
    assert_eq!(req.metadata().get("authorization").unwrap(), "Bearer tok-1");
}
