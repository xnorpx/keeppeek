use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use keeppeek_desktop::{
    config_store::{self, ConfigStoreError, TargetSource},
    navigation::{self, NavigationDecision},
    probe::{self, ProbeError, ProbeOptions},
    server_url::{DEFAULT_SERVER_ORIGIN, MAX_SERVER_ORIGIN_BYTES, ServerOrigin, ServerOriginError},
};
use url::Url;

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after UNIX_EPOCH")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "keeppeek-desktop-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp directory must be creatable");
    dir
}

fn spawn_single_response_server(
    response: Vec<u8>,
    delay: Duration,
) -> (ServerOrigin, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener must bind");
    let address = listener.local_addr().expect("listener must have address");
    let origin = ServerOrigin::parse_canonical(&format!("http://127.0.0.1:{}", address.port()))
        .expect("loopback origin must be canonical");
    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request_buf = [0_u8; 1024];
            let _ = stream.read(&mut request_buf);
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        }
    });
    (origin, handle)
}

#[test]
fn server_origin_parses_valid_inputs_and_rejects_invalid_boundaries() {
    let local = ServerOrigin::parse_user_input("  http://127.0.0.1:8081/ ").unwrap();
    assert_eq!(local.as_str(), "http://127.0.0.1:8081");
    assert!(local.is_loopback());
    assert!(!local.is_https());
    assert_eq!(local.app_entry_url().as_str(), "http://127.0.0.1:8081/");
    assert_eq!(
        local.session_probe_url().as_str(),
        "http://127.0.0.1:8081/auth/session"
    );

    let remote = ServerOrigin::parse_user_input("HTTPS://NVR.EXAMPLE.COM:443/").unwrap();
    assert_eq!(remote.as_str(), "https://nvr.example.com");
    assert!(remote.is_https());
    assert!(!remote.is_loopback());

    let ipv6 = ServerOrigin::parse_user_input("http://[::1]:8081").unwrap();
    assert_eq!(ipv6.as_str(), "http://[::1]:8081");
    assert!(ipv6.is_loopback());

    assert_eq!(
        ServerOrigin::parse_canonical("https://NVR.EXAMPLE.COM").unwrap_err(),
        ServerOriginError::NonCanonical {
            expected: "https://nvr.example.com".to_owned(),
        }
    );

    let oversized = format!(
        "https://{}.example.com",
        "a".repeat(MAX_SERVER_ORIGIN_BYTES)
    );
    assert_eq!(
        ServerOrigin::parse_user_input(&oversized).unwrap_err(),
        ServerOriginError::InvalidLength
    );
    assert_eq!(
        ServerOrigin::parse_user_input("").unwrap_err(),
        ServerOriginError::InvalidLength
    );
    assert_eq!(
        ServerOrigin::parse_user_input("http://127.0.0.1:8081\n").unwrap_err(),
        ServerOriginError::ControlCharacter
    );
    assert_eq!(
        ServerOrigin::parse_user_input("http://127.0.0.1\t:8081").unwrap_err(),
        ServerOriginError::ControlCharacter
    );
    assert_eq!(
        ServerOrigin::parse_user_input("file:///etc/passwd").unwrap_err(),
        ServerOriginError::UnsupportedScheme("file".to_owned())
    );
    assert_eq!(
        ServerOrigin::parse_user_input("javascript:alert(1)").unwrap_err(),
        ServerOriginError::UnsupportedScheme("javascript".to_owned())
    );
    assert_eq!(
        ServerOrigin::parse_user_input("http://admin:secret@127.0.0.1:8081").unwrap_err(),
        ServerOriginError::CredentialsNotAllowed
    );
    assert_eq!(
        ServerOrigin::parse_user_input("http://127.0.0.1:8081/settings").unwrap_err(),
        ServerOriginError::ExtraUrlComponents
    );
    assert_eq!(
        ServerOrigin::parse_user_input("http://127.0.0.1:8081?token=1").unwrap_err(),
        ServerOriginError::ExtraUrlComponents
    );
    assert_eq!(
        ServerOrigin::parse_user_input("http://127.0.0.1:8081#frag").unwrap_err(),
        ServerOriginError::ExtraUrlComponents
    );
    assert_eq!(
        ServerOrigin::parse_user_input("http://127.0.0.1:0").unwrap_err(),
        ServerOriginError::InvalidPort
    );
    assert!(matches!(
        ServerOrigin::parse_user_input("http://0.0.0.0:8081"),
        Err(ServerOriginError::InvalidHost(_))
    ));
    assert!(matches!(
        ServerOrigin::parse_user_input("http://[::]:8081"),
        Err(ServerOriginError::InvalidHost(_))
    ));
}

#[test]
fn server_origin_maps_wildcard_server_bind_to_loopback() {
    let origin = ServerOrigin::from_server_bind("0.0.0.0", 9090).unwrap();
    assert_eq!(origin.as_str(), "http://127.0.0.1:9090");

    let ipv6_any = ServerOrigin::from_server_bind("::", 8081).unwrap();
    assert_eq!(ipv6_any.as_str(), "http://127.0.0.1:8081");

    let explicit_v6 = ServerOrigin::from_server_bind("::1", 8081).unwrap();
    assert_eq!(explicit_v6.as_str(), "http://[::1]:8081");

    assert_eq!(
        ServerOrigin::from_server_bind("127.0.0.1", 0).unwrap_err(),
        ServerOriginError::InvalidPort
    );
}

#[test]
fn navigation_policy_allows_only_launcher_and_active_server_origin() {
    let active = ServerOrigin::parse_canonical("https://nvr.example.com:8443").unwrap();

    for launcher in [
        "tauri://localhost/index.html",
        "http://tauri.localhost/index.html?manual=1",
        "https://tauri.localhost/index.html",
    ] {
        let url = Url::parse(launcher).unwrap();
        assert_eq!(
            navigation::decide_navigation(&url, Some(&active)),
            NavigationDecision::AllowLauncher
        );
        assert!(navigation::decide_navigation(&url, None).is_allowed());
    }

    for same_server in [
        "https://nvr.example.com:8443/",
        "https://nvr.example.com:8443/settings?tab=access",
        "https://nvr.example.com:8443/keep#moment",
    ] {
        let url = Url::parse(same_server).unwrap();
        assert_eq!(
            navigation::decide_navigation(&url, Some(&active)),
            NavigationDecision::AllowConfiguredServer
        );
        assert_eq!(
            navigation::decide_navigation(&url, None),
            NavigationDecision::BlockUntrustedOrigin
        );
    }

    for untrusted_scheme in [
        "file:///C:/Windows/System32/cmd.exe",
        "javascript:alert(1)",
        "data:text/html,<h1>untrusted</h1>",
        "ftp://nvr.example.com/file",
        "tauri://evil.example/index.html",
    ] {
        let url = Url::parse(untrusted_scheme).unwrap();
        assert_eq!(
            navigation::decide_navigation(&url, Some(&active)),
            NavigationDecision::BlockUntrustedScheme
        );
    }

    for untrusted_origin in [
        "http://nvr.example.com:8443/",
        "https://nvr.example.com/",
        "https://nvr.example.com:8444/",
        "https://nvr.example.com.evil.example:8443/",
        "https://user:pass@nvr.example.com:8443/",
        "https://evil.example/",
    ] {
        let url = Url::parse(untrusted_origin).unwrap();
        assert_eq!(
            navigation::decide_navigation(&url, Some(&active)),
            NavigationDecision::BlockUntrustedOrigin,
            "expected {untrusted_origin} to be blocked"
        );
        assert!(!navigation::allow_new_window(&url));
    }
}

#[test]
fn config_store_resolves_precedence_and_preserves_unrelated_sections_and_secrets() {
    let dir = temp_dir("config");
    let config_path = dir.join("config.toml");

    let missing = config_store::resolve_target(&config_path, None, None).unwrap();
    assert_eq!(missing.origin.as_str(), DEFAULT_SERVER_ORIGIN);
    assert_eq!(missing.source, TargetSource::DefaultLocal);

    std::fs::write(
        &config_path,
        r#"
host = "0.0.0.0"
port = 9191
access_key = "{secret:KEEPPEEK_ACCESS_KEY}"

[cameras.front_door]
ip = "192.0.2.10"
password = "{secret:FRONT_PASSWORD}"
"#,
    )
    .unwrap();

    let from_bind = config_store::resolve_target(&config_path, None, None).unwrap();
    assert_eq!(from_bind.origin.as_str(), "http://127.0.0.1:9191");
    assert_eq!(from_bind.source, TargetSource::ConfigServerBind);

    let saved_origin = ServerOrigin::parse_canonical("https://nvr.example.com").unwrap();
    config_store::save_server_url(&config_path, &saved_origin).unwrap();

    let from_desktop = config_store::resolve_target(&config_path, None, None).unwrap();
    assert_eq!(from_desktop.origin, saved_origin);
    assert_eq!(from_desktop.source, TargetSource::ConfigDesktop);

    let from_env =
        config_store::resolve_target(&config_path, None, Some("http://127.0.0.1:8082/")).unwrap();
    assert_eq!(from_env.origin.as_str(), "http://127.0.0.1:8082");
    assert_eq!(from_env.source, TargetSource::Environment);

    let from_cli = config_store::resolve_target(
        &config_path,
        Some("http://127.0.0.1:8083"),
        Some("http://127.0.0.1:8082"),
    )
    .unwrap();
    assert_eq!(from_cli.origin.as_str(), "http://127.0.0.1:8083");
    assert_eq!(from_cli.source, TargetSource::CliArgument);

    let raw_saved = std::fs::read_to_string(&config_path).unwrap();
    assert!(raw_saved.contains("{secret:KEEPPEEK_ACCESS_KEY}"));
    assert!(raw_saved.contains("{secret:FRONT_PASSWORD}"));
    assert!(raw_saved.contains("192.0.2.10"));
    assert!(raw_saved.contains("server_url = \"https://nvr.example.com\""));

    std::fs::write(
        &config_path,
        r#"
[desktop]
server_url = "https://nvr.example.com"
unknown_key = true
"#,
    )
    .unwrap();
    assert!(matches!(
        config_store::resolve_target(&config_path, None, None),
        Err(ConfigStoreError::InvalidDesktopSection(_))
    ));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn probe_accepts_valid_keeppeek_session_and_classifies_failures() {
    let valid_body = r#"{"local":true,"bearer_enabled":false,"methods":[],"identity":{"id":"local-administrator","display_name":"Local Administrator","role":"administrator"},"csrf_token":null}"#;
    let valid_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        valid_body.len(),
        valid_body
    );
    let (origin, handle) =
        spawn_single_response_server(valid_response.into_bytes(), Duration::ZERO);
    let probe = probe::probe_server(&origin, ProbeOptions::default()).unwrap();
    handle.join().unwrap();
    assert_eq!(probe.origin, origin);
    assert!(probe.local);
    assert!(!probe.bearer_enabled);
    assert_eq!(probe.method_count, 0);

    let https_required =
        b"HTTP/1.1 426 Upgrade Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (origin, handle) = spawn_single_response_server(https_required.to_vec(), Duration::ZERO);
    let err = probe::probe_server(&origin, ProbeOptions::default()).unwrap_err();
    handle.join().unwrap();
    assert_eq!(err, ProbeError::HttpsRequired);
    assert!(err.recovery_message(&origin).contains("https://"));

    let redirect = b"HTTP/1.1 302 Found\r\nLocation: https://evil.example/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (origin, handle) = spawn_single_response_server(redirect.to_vec(), Duration::ZERO);
    let err = probe::probe_server(&origin, ProbeOptions::default()).unwrap_err();
    handle.join().unwrap();
    assert_eq!(err, ProbeError::UnexpectedRedirect { status: 302 });

    let non_keeppeek = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 13\r\nConnection: close\r\n\r\n<html></html>";
    let (origin, handle) = spawn_single_response_server(non_keeppeek.to_vec(), Duration::ZERO);
    let err = probe::probe_server(&origin, ProbeOptions::default()).unwrap_err();
    handle.join().unwrap();
    assert!(matches!(err, ProbeError::NotKeepPeekServer { .. }));

    let slow = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (origin, handle) = spawn_single_response_server(slow.to_vec(), Duration::from_millis(900));
    let err = probe::probe_server(
        &origin,
        ProbeOptions {
            timeout_ms: 500,
            max_response_bytes: 1024,
        },
    )
    .unwrap_err();
    handle.join().unwrap();
    assert_eq!(err, ProbeError::TimedOut { timeout_ms: 500 });

    let closed_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed_listener.local_addr().unwrap().port();
    drop(closed_listener);
    let closed_origin =
        ServerOrigin::parse_canonical(&format!("http://127.0.0.1:{closed_port}")).unwrap();
    let err = probe::probe_server(&closed_origin, ProbeOptions::default()).unwrap_err();
    assert!(matches!(err, ProbeError::Unreachable { .. }));
    assert!(err.recovery_message(&closed_origin).contains("Retry"));
}

#[test]
fn tauri_security_configuration_and_capabilities_enforce_least_privilege() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tauri_conf: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(manifest_dir.join("tauri.conf.json")).unwrap(),
    )
    .unwrap();

    assert_eq!(
        tauri_conf["app"]["withGlobalTauri"],
        serde_json::json!(false)
    );
    let csp = tauri_conf["app"]["security"]["csp"]
        .as_str()
        .expect("tauri.conf.json must define an explicit CSP");
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("object-src 'none'"));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(!csp.contains("unsafe-inline"));
    assert!(!csp.contains("unsafe-eval"));
    assert!(!csp.contains('*'));

    let capability: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(manifest_dir.join("capabilities/default.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(capability["local"], serde_json::json!(true));
    assert!(
        capability.get("remote").is_none(),
        "remote origins must never receive native Tauri capabilities"
    );
    let permissions = capability["permissions"]
        .as_array()
        .expect("capabilities must declare explicit permissions");
    for permission in permissions {
        let name = permission
            .as_str()
            .expect("permission entry must be a string");
        assert!(
            !name.starts_with("shell:")
                && !name.starts_with("fs:")
                && !name.starts_with("http:")
                && !name.starts_with("process:")
                && name != "core:default",
            "overly broad or privileged permission is forbidden: {name}"
        );
    }

    let launcher_html = std::fs::read_to_string(manifest_dir.join("frontend/index.html")).unwrap();
    assert!(
        !launcher_html.contains("<script>") && !launcher_html.contains("<style>"),
        "launcher HTML must not contain inline scripts or styles under strict CSP"
    );
}
