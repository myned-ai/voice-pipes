use super::*;

#[test]
fn encrypted_schemes_always_pass() {
    for url in [
        "https://api.openai.com/v1",
        "wss://api.elevenlabs.io",
        "https://example.com",
    ] {
        assert_eq!(check_credential_endpoint(url, true, false), Ok(()), "{url}");
    }
}

#[test]
fn plain_text_to_a_local_host_passes() {
    for url in [
        "http://localhost:8080/v1",
        "http://127.0.0.1:8000",
        "http://[::1]:8000",
        "http://10.1.2.3",
        "http://172.16.0.9",
        "http://172.31.255.1",
        "http://192.168.1.20:8091",
        "http://169.254.10.10",
        "http://[fd12:3456::1]",
        "http://[fe80::1]",
        // Container and LAN names: the wizard's own sidecars.
        "http://vibevoice:8091/v1",
        "http://llama-server:8081/v1",
        "http://api.localhost",
        "http://host.docker.internal:8080/v1",
        "http://printer.local",
        "http://nas.home.arpa",
        "http://box.lan",
        "ws://localhost:9000",
    ] {
        assert_eq!(check_credential_endpoint(url, true, false), Ok(()), "{url}");
    }
}

#[test]
fn plain_text_to_an_internet_host_is_refused() {
    for (url, host) in [
        ("http://api.openai.com/v1", "api.openai.com"),
        (
            "ws://streaming.assemblyai.com/v3/ws",
            "streaming.assemblyai.com",
        ),
        ("http://172.32.0.1", "172.32.0.1"),
        ("http://8.8.8.8", "8.8.8.8"),
        ("http://[2001:db8::1]", "[2001:db8::1]"),
    ] {
        assert_eq!(
            check_credential_endpoint(url, true, false),
            Err(EndpointError::Plaintext(host.into())),
            "{url}"
        );
    }
}

#[test]
fn the_operator_can_opt_in_and_keyless_requests_are_not_checked() {
    assert_eq!(
        check_credential_endpoint("http://api.example.com", true, true),
        Ok(())
    );
    assert_eq!(
        check_credential_endpoint("http://api.example.com", false, false),
        Ok(())
    );
}

#[test]
fn nonsense_is_refused_without_echoing_the_url() {
    assert_eq!(
        check_credential_endpoint("ftp://files.example.com", true, false),
        Err(EndpointError::UnsupportedScheme("ftp".into()))
    );
    let err = check_credential_endpoint("not a url ?key=sk-123", true, false).unwrap_err();
    assert_eq!(err, EndpointError::Invalid);
    assert!(!err.to_string().contains("sk-123"));
}
