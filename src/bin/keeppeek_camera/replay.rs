use clap::Args;
use keeppeek::shutdown::Shutdown;
use std::{
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

#[path = "../../../crates/test-camera/src/replay.rs"]
mod engine;

use engine::{ReplayCameraServer, ReplayServerOptions};

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// CLI arguments for `keeppeek-camera replay`.
#[derive(Debug, Args)]
pub struct Cli {
    /// Path to a keeppeek.camera-repro.v1 JSON fixture file.
    #[arg(long)]
    fixture: PathBuf,

    /// Loopback or explicit socket address to bind.
    #[arg(long, default_value_t = default_bind_addr())]
    bind: SocketAddr,

    /// Exit automatically after all scripted exchanges finish one pass.
    #[arg(long, conflicts_with = "loop_replay")]
    once: bool,

    /// Repeat scripted exchanges, RTP packets, and Baichuan frames continuously.
    #[arg(long = "loop", conflicts_with = "once")]
    loop_replay: bool,
}

const fn default_bind_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

/// Runs the `keeppeek-camera replay` subcommand until Ctrl+C or `--once` completion.
pub fn run(cli: Cli) -> anyhow::Result<()> {
    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    ctrlc::set_handler(move || signal.cancel())
        .map_err(|error| anyhow::anyhow!("unable to install shutdown signal handler: {error}"))?;
    let mut stdout = std::io::stdout().lock();
    run_with_shutdown(cli, &shutdown, &mut stdout)
}

fn run_with_shutdown(cli: Cli, shutdown: &Shutdown, out: &mut impl Write) -> anyhow::Result<()> {
    let options = ReplayServerOptions {
        bind_addr: cli.bind,
        loop_replay: cli.loop_replay,
    };
    let server = ReplayCameraServer::from_file_with_options(&cli.fixture, options)?;
    print_bound_urls(&server, out)?;
    while !shutdown.wait_timeout(POLL_INTERVAL) {
        if cli.once && server.is_completed() {
            break;
        }
    }
    Ok(())
}

fn print_bound_urls(server: &ReplayCameraServer, out: &mut impl Write) -> anyhow::Result<()> {
    if let Some(url) = server.http_url() {
        writeln!(out, "{url}")?;
    }
    if let Some(url) = server.rtsp_url() {
        writeln!(out, "{url}")?;
    }
    if let Some(url) = server.reo_url() {
        writeln!(out, "{url}")?;
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::io::Read;
    use std::net::TcpStream;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: TestSubcommand,
    }

    #[derive(Debug, clap::Subcommand)]
    enum TestSubcommand {
        Replay(Cli),
    }

    #[test]
    fn cli_parses_once_and_loop_flags_and_rejects_conflict() {
        let parsed = TestCli::try_parse_from([
            "keeppeek-camera",
            "replay",
            "--fixture",
            "repro.json",
            "--bind",
            "127.0.0.1:18554",
            "--once",
        ])
        .unwrap();
        let TestSubcommand::Replay(cli) = parsed.command;
        assert_eq!(cli.fixture, PathBuf::from("repro.json"));
        assert_eq!(cli.bind, "127.0.0.1:18554".parse::<SocketAddr>().unwrap());
        assert!(cli.once);
        assert!(!cli.loop_replay);

        let looped = TestCli::try_parse_from([
            "keeppeek-camera",
            "replay",
            "--fixture",
            "repro.json",
            "--loop",
        ])
        .unwrap();
        let TestSubcommand::Replay(loop_cli) = looped.command;
        assert!(loop_cli.loop_replay);
        assert!(!loop_cli.once);

        assert!(
            TestCli::try_parse_from([
                "keeppeek-camera",
                "replay",
                "--fixture",
                "repro.json",
                "--once",
                "--loop",
            ])
            .is_err()
        );
    }

    #[test]
    fn prints_bound_urls_and_serves_http_smoke() {
        let fixture_path = std::env::temp_dir().join(format!(
            "keeppeek-camera-cli-replay-{}.json",
            std::process::id()
        ));
        let fixture = serde_json::json!({
            "schema_version": "keeppeek.camera-repro.v1",
            "camera": {
                "name": "cli-smoke",
                "protocol": "isapi",
                "anonymized": true,
                "credentials_stripped": true,
                "base_host": "192.0.2.5"
            },
            "http": {
                "exchanges": [{
                    "match_method": "GET",
                    "match_path_contains": "/ISAPI/System/status",
                    "status": 200,
                    "headers": [["Content-Type", "application/xml"]],
                    "body_text": "<DeviceStatus><status>ok</status></DeviceStatus>"
                }]
            }
        });
        std::fs::write(&fixture_path, fixture.to_string()).unwrap();
        let server = ReplayCameraServer::from_file(&fixture_path).unwrap();
        let http_url = server.http_url().unwrap();
        let mut out = Vec::new();
        print_bound_urls(&server, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains(&http_url));
        let addr: SocketAddr = http_url.strip_prefix("http://").unwrap().parse().unwrap();
        let mut conn = TcpStream::connect(addr).unwrap();
        conn.write_all(
            b"GET /ISAPI/System/status HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
        let mut resp = String::new();
        conn.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("<DeviceStatus><status>ok</status></DeviceStatus>"));
        let _ = std::fs::remove_file(&fixture_path);
    }

    #[test]
    fn run_once_exits_cleanly_on_empty_fixture() {
        let empty_path = std::env::temp_dir().join(format!(
            "keeppeek-camera-cli-empty-{}.json",
            std::process::id()
        ));
        let empty_fixture = serde_json::json!({
            "schema_version": "keeppeek.camera-repro.v1",
            "camera": {
                "protocol": "rtsp",
                "anonymized": true,
                "credentials_stripped": true,
                "base_host": "192.0.2.5"
            }
        });
        std::fs::write(&empty_path, empty_fixture.to_string()).unwrap();
        let shutdown = Shutdown::new();
        let mut empty_out = Vec::new();
        run_with_shutdown(
            Cli {
                fixture: empty_path.clone(),
                bind: default_bind_addr(),
                once: true,
                loop_replay: false,
            },
            &shutdown,
            &mut empty_out,
        )
        .unwrap();
        let _ = std::fs::remove_file(&empty_path);
    }

    #[test]
    fn replays_isapi_digest_challenge_to_real_isapi_client() {
        let fixture = engine::CameraReproFixture {
            schema_version: engine::REPRO_SCHEMA_VERSION.to_owned(),
            camera: engine::ReproCameraInfo {
                name: Some("isapi-repro".to_owned()),
                protocol: engine::ReproProtocol::Isapi,
                anonymized: true,
                credentials_stripped: true,
                base_host: "192.0.2.6".to_owned(),
            },
            rtsp: None,
            http: Some(engine::HttpReproFixture {
                exchanges: vec![
                    engine::HttpExchangeFixture {
                        match_method: Some("GET".to_owned()),
                        match_path_contains: Some("/ISAPI/System/deviceInfo".to_owned()),
                        match_soap_action: None,
                        match_body_contains: None,
                        status: 401,
                        status_reason: Some("Unauthorized".to_owned()),
                        headers: vec![(
                            "WWW-Authenticate".to_owned(),
                            r#"Digest realm="repro", nonce="0123456789abcdef", algorithm=MD5, qop="auth""#.to_owned(),
                        )],
                        body_text: None,
                        body_base64: None,
                        delay_ms: None,
                    },
                    engine::HttpExchangeFixture {
                        match_method: Some("GET".to_owned()),
                        match_path_contains: Some("/ISAPI/System/deviceInfo".to_owned()),
                        match_soap_action: None,
                        match_body_contains: None,
                        status: 200,
                        status_reason: Some("OK".to_owned()),
                        headers: vec![("Content-Type".to_owned(), "application/xml".to_owned())],
                        body_text: Some(
                            "<DeviceInfo><model>DS-REPRO</model><deviceName>ReproCam</deviceName></DeviceInfo>".to_owned(),
                        ),
                        body_base64: None,
                        delay_ms: None,
                    },
                ],
            }),
            reo_proto: None,
        };
        let server = ReplayCameraServer::start(fixture).unwrap();
        let mut client = ::isapi::blocking::Client::new(
            server.http_url().unwrap(),
            ::isapi::Credentials::new("admin", "secret"),
        )
        .unwrap();
        let query = ::isapi::management::DeviceInfo::query().unwrap();
        let info = client.query(&query).unwrap();
        assert_eq!(info.model(), "DS-REPRO");
        assert_eq!(info.name(), Some("ReproCam"));
        assert_eq!(server.recorded_requests().len(), 2);
        assert!(server.is_completed());
    }
}
