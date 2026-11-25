use std::{
    fs,
    io::Read,
    net::TcpListener,
    process::{Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_keeppeek"))
        .args(args)
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn help_version_and_configuration_keep_plain_output_with_a_dashboard_request() {
    for args in [
        &["--tui", "always", "--help"][..],
        &["--tui", "always", "--version"],
        &["--tui", "always", "config", "--help"],
    ] {
        let output = run(args);
        assert!(output.status.success(), "{args:?}");
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.contains(&b'\x1b'));
    }
    let help = String::from_utf8(run(&["--help"]).stdout).unwrap();
    assert!(
        ["--tui", "auto", "always", "never"]
            .iter()
            .all(|option| help.contains(option))
    );
}

#[test]
fn the_foreground_cli_rejects_an_unknown_dashboard_mode() {
    let output = run(&["--tui", "invalid-dashboard-setting"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .starts_with("error:")
    );
}

#[test]
fn redirected_foreground_server_keeps_plain_logs_and_reports_always_fallback() {
    for (mode, expect_diagnostic) in [("auto", false), ("never", false), ("always", true)] {
        let port = TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let directory =
            std::env::temp_dir().join(format!("keeppeek-tui-proc-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("config.toml"),
            format!("host = \"127.0.0.1\"\nport = {port}\n"),
        )
        .unwrap();

        let mut child = Command::new(env!("CARGO_BIN_EXE_keeppeek"))
            .args(["--config", "config.toml", "--tui", mode])
            .current_dir(&directory)
            .env("APPDATA", &directory)
            .env("HOME", &directory)
            .env("XDG_CONFIG_HOME", &directory)
            .env("NO_COLOR", "1")
            .env_remove("CLICOLOR_FORCE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let mut stderr_pipe = child.stderr.take().unwrap();
        let (stderr_tx, stderr_rx) = mpsc::channel();
        let stderr_reader = thread::spawn(move || {
            let mut buffer = [0u8; 1024];
            let mut all = Vec::new();
            while let Ok(count) = stderr_pipe.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                let chunk = buffer[..count].to_vec();
                all.extend_from_slice(&chunk);
                let _ = stderr_tx.send(chunk);
            }
            all
        });

        let client = keeppeek::client::KeepPeekClient::new(&format!("http://127.0.0.1:{port}"));
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(health) = client.health()
                && health.status == "ok"
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "redirected server with --tui {mode} did not report healthy"
            );
            thread::sleep(Duration::from_millis(50));
        }

        let diagnostic =
            "KeepPeek dashboard requires an interactive terminal. Continuing with plain logs.";
        if expect_diagnostic {
            let mut observed = Vec::new();
            let diag_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if String::from_utf8_lossy(&observed).contains(diagnostic) {
                    break;
                }
                let remaining = diag_deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "--tui {mode} did not emit the fallback diagnostic: {}",
                    String::from_utf8_lossy(&observed)
                );
                match stderr_rx.recv_timeout(remaining) {
                    Ok(chunk) => observed.extend_from_slice(&chunk),
                    Err(_) => panic!(
                        "--tui {mode} closed stderr before emitting the fallback diagnostic: {}",
                        String::from_utf8_lossy(&observed)
                    ),
                }
            }
        }
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        let stderr_bytes = stderr_reader.join().unwrap();
        let _ = fs::remove_dir_all(&directory);

        assert!(
            !output.stdout.contains(&b'\x1b'),
            "--tui {mode} emitted terminal escape on stdout"
        );
        assert!(
            !stderr_bytes.contains(&b'\x1b'),
            "--tui {mode} emitted terminal escape on stderr"
        );
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        assert_eq!(
            stderr.contains(diagnostic),
            expect_diagnostic,
            "unexpected fallback diagnostic state for --tui {mode}: {stderr}"
        );
    }
}
