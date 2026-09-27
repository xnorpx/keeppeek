//! Opt-in real HTTP/WebRTC owner for the isolated TLS browser authentication tests.

use super::*;
use crate::access::{external, oidc};
use std::io::BufRead;

struct BrowserFixture {
    state: ServerState,
    provider: oidc::FixtureProvider,
    directory: PathBuf,
    origin: String,
    administrator: bool,
    available: bool,
}

impl BrowserFixture {
    fn new(origin: String) -> Self {
        let parsed = Url::parse(&origin).unwrap();
        assert_eq!(parsed.scheme(), "https");
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_eq!(parsed.origin().ascii_serialization(), origin);
        let directory =
            std::env::temp_dir().join(format!("keeppeek-browser-auth-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let mut state = ServerState::empty();
        state.camera_config_path = Some(directory.join("config.toml"));
        state.allowed_origins = Arc::new(HashSet::from([origin.clone()]));
        state.network_access = NetworkAccessPolicy::new(
            vec!["127.0.0.0/8".parse().unwrap(), "::1/128".parse().unwrap()],
            vec!["127.0.0.1/32".parse().unwrap()],
        );
        let (logging, dispatch) = LoggingService::for_test(
            crate::logging::LogFilterFile::new(directory.join("log-filter")),
            "info",
        );
        tracing::dispatcher::set_global_default(dispatch).unwrap();
        state.logging = Some(logging);
        let mut fixture = Self {
            state,
            provider: oidc::FixtureProvider::for_browser(&origin),
            directory,
            origin,
            administrator: false,
            available: true,
        };
        fixture.configure(false);
        fixture.state.access_manager = AccessManager::open_with_config_update(
            fixture.state.camera_config_path.as_ref().unwrap(),
            AccessKey::unset(),
            fixture.state.config_update.clone(),
        )
        .unwrap();
        fixture
    }

    fn configure(&self, proxy: bool) {
        let mut root = toml::Table::new();
        root.insert(
            "external_auth".into(),
            toml::Value::try_from(self.settings(proxy)).unwrap(),
        );
        let path = self.state.camera_config_path.as_ref().unwrap();
        config::write_configuration_table(path, &root).unwrap();
        let config = config::load_config(path).unwrap();
        let next = authentication::Registry::new(&config, self.state.api_session_policy);
        authentication::activate_configuration(&self.state, Some(next));
        *self.state.oidc_cache.lock().unwrap() = oidc::Cache::default();
        if !proxy {
            self.state
                .oidc_cache
                .lock()
                .unwrap()
                .get(1, "company", Instant::now(), || {
                    oidc::Provider::discover(&self.provider.config, self.provider.transport.clone())
                })
                .unwrap();
        }
        expire_api_sessions(&self.state);
    }

    fn settings(&self, proxy: bool) -> external::Config {
        let method = if proxy {
            external::Method::Proxy(external::Proxy {
                trusted_peers: vec!["127.0.0.1/32".parse().unwrap()],
                subject_header: "X-KeepPeek-Subject".into(),
                role_header: "X-KeepPeek-Role".into(),
                name_header: Some("X-KeepPeek-Name".into()),
                secret_header: None,
                shared_secret: None,
            })
        } else {
            external::Method::Oidc(self.provider.config.clone())
        };
        let claim = if proxy { "role" } else { "groups" };
        let mappings = [
            (AccessRole::Administrator, "administrators", "administrator"),
            (AccessRole::User, "viewers", "user"),
        ]
        .into_iter()
        .map(|(role, oidc, proxy_value)| external::Mapping {
            claim: claim.into(),
            value: if proxy { proxy_value } else { oidc }.into(),
            role,
            camera_access: (role == AccessRole::User)
                .then(crate::access::CameraAccess::unrestricted),
        })
        .collect();
        external::Config {
            allowed_origins: vec![self.origin.clone()],
            bearer_enabled: false,
            bearer_transition_until_ms: None,
            providers: vec![external::Provider {
                id: "company".into(),
                name: "Fixture sign-in".into(),
                mappings,
                method,
            }],
        }
    }

    fn command(&mut self, command: &str) -> bool {
        match command {
            "stop" => return false,
            "outage" => self.available = false,
            "healthy" => self.available = true,
            "user" => self.administrator = false,
            "administrator" => self.administrator = true,
            "revoke" => revoke_browsers(&self.state),
            "proxy" => self.configure(true),
            "oidc" => self.configure(false),
            "audit" => println!(
                "KEEPPEEK_AUTH_AUDIT {}",
                serde_json::json!({
                    "audit": self.state.access_manager.list_audit(1_000),
                    "logs": self.state.logging.as_ref().unwrap().snapshot(None, 1_000),
                })
            ),
            _ => panic!("unknown browser fixture command"),
        }
        self.provider
            .browser_mode(self.available, self.administrator);
        println!("KEEPPEEK_AUTH_ACK {command}");
        true
    }
}

impl Drop for BrowserFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn revoke_browsers(state: &ServerState) {
    use proto::external_authentication_command::Action;
    let result = administer(
        state,
        Action::ListSessions(proto::ListBrowserSessions {
            page_size: Some(32),
            page_token: String::new(),
        }),
    );
    let control_ok::Result::ExternalAuthenticationResult(result) = result else {
        panic!("unexpected administration response")
    };
    let Some(proto::external_authentication_result::Result::Sessions(result)) = result.result
    else {
        panic!("unexpected session response")
    };
    assert!(result.next_page_token.is_empty());
    for session in result.sessions {
        administer(
            state,
            Action::RevokeSession(proto::RevokeBrowserSession {
                session_id: session.session_id,
            }),
        );
    }
}

fn administer(
    state: &ServerState,
    action: proto::external_authentication_command::Action,
) -> control_ok::Result {
    authentication::admin::dispatch(
        state,
        SessionId::from_u64(0),
        &ApiPrincipal::local("127.0.0.1".parse().unwrap()),
        ClientClassificationReason::DirectLocal,
        proto::ExternalAuthenticationCommand {
            action: Some(action),
        },
    )
    .unwrap()
}

fn browser_certificate(directory: &Path) -> (PathBuf, PathBuf) {
    let params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    let certificate_path = directory.join("tls-cert.pem");
    let key_path = directory.join("tls-key.pem");
    for (path, bytes) in [
        (&certificate_path, cert.pem()),
        (&key_path, key.serialize_pem()),
    ] {
        let mut file = crate::backup::create_private_file(path).unwrap();
        file.write_all(bytes.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
    (certificate_path, key_path)
}

fn commands() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::sync_channel(4);
    std::thread::spawn(move || {
        let input = std::io::BufReader::new(std::io::stdin().take(16 * 1024));
        for line in input.lines().take(64) {
            let line = line.unwrap();
            assert!(line.len() <= 1_024);
            let command: serde_json::Value = serde_json::from_str(&line).unwrap();
            if tx
                .send(command["command"].as_str().unwrap().to_owned())
                .is_err()
            {
                break;
            }
        }
    });
    rx
}

#[test]
#[ignore = "local TLS browser harness; launched only by the authentication E2E fixture"]
fn issue123_browser_fixture() {
    let origin = std::env::var("KEEPPEEK_AUTH_E2E_ORIGIN").expect("browser origin is required");
    let mut fixture = BrowserFixture::new(origin);
    let (certificate_path, key_path) = browser_certificate(&fixture.directory);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let shutdown = Shutdown::new();
    let (mut router, router_tx) = crate::runtime::Router::new().unwrap();
    let (ready, bound) = mpsc::sync_channel(1);
    let worker_state = fixture.state.clone();
    let worker_shutdown = shutdown.clone();
    let server = std::thread::spawn(move || {
        serve_with_state_on_listener_ready(
            listener,
            worker_shutdown,
            router_tx,
            worker_state,
            ready,
        )
        .unwrap()
    });
    let backend = format!(
        "http://{}",
        bound.recv_timeout(Duration::from_secs(10)).unwrap()
    );
    println!(
        "KEEPPEEK_AUTH_FIXTURE {}",
        serde_json::json!({
            "backend": backend, "issuer": fixture.provider.config.issuer,
            "certificate_path": certificate_path, "key_path": key_path,
        })
    );
    let commands = commands();
    let deadline = Instant::now() + Duration::from_secs(600);
    while Instant::now() < deadline {
        router
            .wait_and_drain(Some(Duration::from_millis(10)))
            .unwrap();
        match commands.try_recv() {
            Ok(command) if !fixture.command(&command) => break,
            Ok(_) | Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
    }
    shutdown.cancel();
    server.join().unwrap();
}
