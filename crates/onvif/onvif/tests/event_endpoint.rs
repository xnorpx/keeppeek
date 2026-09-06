use onvif::event::Endpoint;

#[test]
fn subscription_endpoints_are_camera_bound_and_wildcards_preserve_paths() {
    let camera = Endpoint::new("http://192.0.2.20:8080/onvif/events").unwrap();
    assert_eq!(
        camera
            .resolve("http://0.0.0.0/onvif/subscription?id=private")
            .unwrap()
            .as_str(),
        "http://192.0.2.20:8080/onvif/subscription?id=private"
    );
    assert_eq!(
        camera.resolve("/onvif/subscription/7").unwrap().as_str(),
        "http://192.0.2.20:8080/onvif/subscription/7"
    );
    assert_eq!(
        camera
            .resolve("http://192.0.2.20:8080/onvif/subscription/7")
            .unwrap()
            .as_str(),
        "http://192.0.2.20:8080/onvif/subscription/7"
    );
    for address in [
        "http://127.0.0.1/onvif/sub",
        "http://169.254.169.254/metadata",
        "http://192.0.2.21/onvif/sub",
        "http://example.org/sub",
        "file:///etc/passwd",
        "http://user:secret@192.0.2.20/onvif/sub",
        "http://192.0.2.20/sub#fragment",
        "http://192.0.2.20:0/sub",
    ] {
        assert!(
            camera.resolve(address).is_err(),
            "unsafe advertised endpoint: {address}"
        );
    }
    assert!(
        !format!(
            "{:?}",
            camera.resolve("/subscription?token=private").unwrap()
        )
        .contains("private")
    );
}

#[test]
fn configured_loopback_is_allowed_but_tls_is_never_downgraded() {
    let camera = Endpoint::new("https://127.0.0.1:8443/onvif/events").unwrap();
    assert!(camera.resolve("http://127.0.0.1:8443/sub").is_err());
    assert_eq!(
        camera.resolve("https://0.0.0.0/sub").unwrap().as_str(),
        "https://127.0.0.1:8443/sub"
    );
    for address in [
        "http://0.0.0.0/events",
        "http://224.0.0.1/events",
        "http://192.0.2.20/events?token=secret",
        "http://192.0.2.20\\@127.0.0.1/events",
    ] {
        assert!(Endpoint::new(address).is_err());
    }
}
