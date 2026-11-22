use super::*;
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::Path,
    process::{Command, Stdio},
};

const TEST: &str = "shares::tests::restart_process::restart_preserves_consumption_and_revocation_but_invalidates_sessions";

#[derive(Deserialize, Serialize)]
struct Client {
    id: Uuid,
    token: String,
    session: String,
    unix_ms: i64,
    origin: String,
    policy_revision: u64,
}

fn verify_after_restart(path: &Path, phase: &str) {
    // Only the client knows these values. Carry them through stdin, outside process
    // arguments, the recorder database and captured test output.
    let client: Client = serde_json::from_reader(io::stdin().lock()).unwrap();
    let now = Clock {
        unix_ms: client.unix_ms,
        monotonic: Instant::now(),
    };
    let context = SessionContext {
        origin: client.origin,
        policy_revision: client.policy_revision,
    };
    let mut store = Store::open(path, now.unix_ms).unwrap();
    let grant = store.get(client.id, now).unwrap().unwrap();
    assert_eq!(grant.views, 1);
    assert_eq!(
        store
            .inspect_session(&client.session, None, &context, now)
            .unwrap_err(),
        Error::Denied
    );
    match phase {
        "after-use" => {
            assert!(grant.revoked_at_ms.is_none());
            let new = store
                .exchange(&client.token, &context, now)
                .unwrap()
                .handle
                .into_string();
            assert_eq!(
                store
                    .begin_delivery(&new, &context, &grant.intent.binding, Mode::Stream, now)
                    .unwrap_err(),
                Error::Denied
            );
            store
                .revoke(grant.id, grant.revision, "withdrawn", now)
                .unwrap();
        }
        "after-revocation" => {
            assert!(grant.revoked_at_ms.is_some());
            assert_eq!(
                store.exchange(&client.token, &context, now).unwrap_err(),
                Error::Denied
            );
        }
        _ => panic!("unknown restart phase"),
    }
    println!("guest-restart-verified");
}

#[test]
fn restart_preserves_consumption_and_revocation_but_invalidates_sessions() {
    if let Some(path) = std::env::var_os("KEEPPEEK_TEST_GUEST_RESTART_DB") {
        verify_after_restart(
            Path::new(&path),
            &std::env::var("KEEPPEEK_TEST_GUEST_RESTART_PHASE").unwrap(),
        );
        return;
    }
    let mut fixture = Fixture::new();
    let (grant, token) = fixture.grant();
    let handle = fixture.session(&token);
    let pending = fixture.begin(&handle, Mode::Stream).unwrap();
    fixture.commit(&pending).unwrap();
    let Fixture {
        store,
        directory,
        now,
        context,
        ..
    } = fixture;
    drop(store);
    let client = Client {
        id: grant.id,
        token,
        session: handle,
        unix_ms: now.unix_ms,
        origin: context.origin,
        policy_revision: context.policy_revision,
    };
    for phase in ["after-use", "after-revocation"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(
                "KEEPPEEK_TEST_GUEST_RESTART_DB",
                directory.0.join("grants.db"),
            )
            .env("KEEPPEEK_TEST_GUEST_RESTART_PHASE", phase)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        serde_json::to_writer(child.stdin.take().unwrap(), &client).unwrap();
        let result = wait_for_child(child);
        assert!(
            result.status.success(),
            "{phase} failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            stdout.contains("guest-restart-verified"),
            "{phase} must report restart verification: {stdout}"
        );
    }
}
