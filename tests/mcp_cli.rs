//! Real-binary CLI integration tests for `keeppeek mcp` and `keeppeek mcp skill`.

use keeppeek::{
    config::{self, mcp::McpConfig},
    runtime::Router,
    server::{ServerState, serve_with_state_on_listener},
    shutdown::Shutdown,
    storage::{RecordingDemand, StorageConfig},
    webrtc::WebRtc,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    io::Write,
    net::TcpListener,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

const BIN: &str = env!("CARGO_BIN_EXE_keeppeek");
const ACCESS_KEY: &str = "550e8400-e29b-41d4-a716-446655440000";
const SENTINEL: &str = "CLI-BINARY-SENTINEL-SECRET-991122";

#[test]
fn keeppeek_mcp_skill_prints_exact_embedded_skill_markdown() {
    let output = Command::new(BIN)
        .args(["mcp", "skill"])
        .output()
        .expect("spawn keeppeek mcp skill");
    assert!(
        output.status.success(),
        "keeppeek mcp skill failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "keeppeek mcp skill must not write warnings to stderr"
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 skill output");
    let expected = include_str!("../skills/keeppeek-mcp/SKILL.md");
    assert_eq!(stdout, expected);
    assert!(stdout.starts_with("---\nname: keeppeek-mcp\n"));
}

#[test]
fn keeppeek_mcp_cli_stdio_authenticates_against_live_server_and_redacts_secrets() {
    let dir = std::env::temp_dir().join(format!("keeppeek-mcp-cli-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.toml");
    let secrets_path = dir.join("secrets.toml");
    std::fs::write(
        &secrets_path,
        format!("CAMERA_PASSWORD = \"{SENTINEL}\"\nKEEPPEEK_ACCESS_KEY = \"{ACCESS_KEY}\"\n"),
    )
    .unwrap();
    std::fs::write(
        &config_path,
        r#"
host = "127.0.0.1"
port = 8081
access_key = "{secret:KEEPPEEK_ACCESS_KEY}"

[access]
local_networks = []
require_secure_remote = false

[mcp]
enabled = true
allow_writes = false

[camera_defaults]
username = "admin"
password = "{secret:CAMERA_PASSWORD}"

[front.door]
ip = "192.0.2.10"
display_name = "Front Door"
"#,
    )
    .unwrap();

    let mut cfg = config::load_config(&config_path).unwrap();
    cfg.mcp = McpConfig {
        enabled: true,
        allow_writes: false,
        ..McpConfig::default()
    };
    let cameras = config::load_cameras(&config_path).unwrap();
    let storage = StorageConfig {
        long_term_path: dir.join("recordings"),
        ..StorageConfig::default()
    };
    let state = ServerState::new(
        &cfg,
        &cameras,
        &HashMap::new(),
        &storage,
        RecordingDemand::new(Duration::from_secs(5)),
        WebRtc::new(),
    )
    .with_camera_config_path(config_path.clone());
    let (_router, router_tx) = Router::new().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = Shutdown::new();
    let server_shutdown = shutdown.clone();
    let server_thread = thread::spawn(move || {
        serve_with_state_on_listener(listener, server_shutdown, router_tx, state).unwrap();
    });

    let mut child = Command::new(BIN)
        .args(["mcp", "--server", &format!("http://{addr}")])
        .env("KEEPPEEK_ACCESS_KEY", ACCESS_KEY)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn keeppeek mcp");

    {
        let stdin = child.stdin.as_mut().expect("stdin pipe");
        let requests = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"list_secret_keys\",\"arguments\":{}}}\n"
        );
        stdin.write_all(requests.as_bytes()).unwrap();
    }
    let output = child.wait_with_output().expect("wait keeppeek mcp");
    assert!(
        output.status.success(),
        "keeppeek mcp exited non-zero: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    assert!(!stdout.contains(SENTINEL));
    assert!(!stdout.contains(ACCESS_KEY));
    let frames: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid single-line JSON-RPC frame"))
        .collect();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(frames[1]["result"]["tools"].as_array().unwrap().len(), 27);
    assert_eq!(
        frames[2]["result"]["structuredContent"]["keys"],
        serde_json::json!(["CAMERA_PASSWORD", "KEEPPEEK_ACCESS_KEY"])
    );

    // Live reload: add a new secret via `keeppeek secret set --stdin` while the server is running.
    let live_sentinel = "LIVE-RELOAD-CLI-SENTINEL-774411";
    let mut set_child = Command::new(BIN)
        .args([
            "secret",
            "set",
            "--config",
            config_path.to_str().unwrap(),
            "--stdin",
            "LIVE_NEW_SECRET",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn keeppeek secret set");
    set_child
        .stdin
        .as_mut()
        .expect("secret stdin")
        .write_all(format!("{live_sentinel}\n").as_bytes())
        .unwrap();
    let set_out = set_child.wait_with_output().expect("wait secret set");
    assert!(
        set_out.status.success(),
        "keeppeek secret set failed: {}",
        String::from_utf8_lossy(&set_out.stderr)
    );
    assert!(!String::from_utf8_lossy(&set_out.stdout).contains(live_sentinel));
    assert!(!String::from_utf8_lossy(&set_out.stderr).contains(live_sentinel));

    let list_out = Command::new(BIN)
        .args(["secret", "list", "--config", config_path.to_str().unwrap()])
        .output()
        .expect("spawn keeppeek secret list");
    assert!(list_out.status.success());
    let list_stdout = String::from_utf8(list_out.stdout).unwrap();
    assert!(list_stdout.contains("LIVE_NEW_SECRET"));
    assert!(!list_stdout.contains(live_sentinel));
    assert!(!list_stdout.contains(SENTINEL));

    // Running MCP server sees LIVE_NEW_SECRET immediately without restart.
    let mut check_child = Command::new(BIN)
        .args(["mcp", "--server", &format!("http://{addr}")])
        .env("KEEPPEEK_ACCESS_KEY", ACCESS_KEY)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn keeppeek mcp live reload check");
    check_child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"list_secret_keys\",\"arguments\":{}}}\n")
        .unwrap();
    let check_out = check_child.wait_with_output().expect("wait live check");
    assert!(check_out.status.success());
    let check_stdout = String::from_utf8(check_out.stdout).unwrap();
    assert!(!check_stdout.contains(live_sentinel));
    let check_frame: Value = serde_json::from_str(check_stdout.trim()).unwrap();
    assert_eq!(
        check_frame["result"]["structuredContent"]["keys"],
        serde_json::json!(["CAMERA_PASSWORD", "KEEPPEEK_ACCESS_KEY", "LIVE_NEW_SECRET"])
    );

    // Invalid key fails with usage exit code 2; remove cleans up cleanly.
    let bad_key = Command::new(BIN)
        .args([
            "secret",
            "set",
            "--config",
            config_path.to_str().unwrap(),
            "--stdin",
            "invalid-key",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("spawn invalid secret key");
    assert_eq!(bad_key.status.code(), Some(2));

    let rm_out = Command::new(BIN)
        .args([
            "secret",
            "remove",
            "--config",
            config_path.to_str().unwrap(),
            "LIVE_NEW_SECRET",
        ])
        .output()
        .expect("spawn keeppeek secret remove");
    assert!(rm_out.status.success());

    // Verify missing credentials fail authentication cleanly when local_networks is empty.
    let mut unauth = Command::new(BIN)
        .args(["mcp", "--server", &format!("http://{addr}")])
        .env_remove("KEEPPEEK_ACCESS_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn unauthenticated keeppeek mcp");
    {
        let stdin = unauth.stdin.as_mut().expect("stdin pipe");
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .unwrap();
    }
    let unauth_out = unauth.wait_with_output().expect("wait unauth");
    assert!(unauth_out.status.success());
    let unauth_frame: Value =
        serde_json::from_str(std::str::from_utf8(&unauth_out.stdout).unwrap().trim()).unwrap();
    assert!(unauth_frame["error"].is_object());

    shutdown.cancel();
    let _ = server_thread.join();
    let _ = std::fs::remove_dir_all(&dir);
}
