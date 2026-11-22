use super::*;

const CHILD_TEST: &str =
    "shares::tests::retention::reopening_purges_the_entire_overdue_audit_before_serving_grants";

#[test]
fn reopening_purges_the_entire_overdue_audit_before_serving_grants() {
    if let Some(path) = std::env::var_os("KEEPPEEK_TEST_AUDIT_REOPEN") {
        let now = std::env::var("KEEPPEEK_TEST_AUDIT_CLOCK")
            .unwrap()
            .parse()
            .unwrap();
        let started = Instant::now();
        let _store = Store::open(std::path::Path::new(&path), now).unwrap();
        println!("audit_open_us={}", started.elapsed().as_micros());
        return;
    }

    verify_reopen(true, 257);
}

#[test]
fn reopening_purges_never_revoked_expired_grants() {
    verify_reopen(false, 2);
}

fn verify_reopen(revoked: bool, count: usize) {
    let mut fixture = Fixture::new();
    seed_history(
        &fixture,
        Seed {
            count,
            terminal: revoked,
            group_creators: false,
            created_ms: fixture.now.unix_ms,
        },
    );
    let cutoff = if revoked {
        fixture.now.unix_ms
    } else {
        fixture.intent.expires_at_ms
    };
    fixture.intent.expires_at_ms += 60_000;
    let (recent, _) = fixture.grant();
    let Fixture {
        store, directory, ..
    } = fixture;
    drop(store);
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env("KEEPPEEK_TEST_AUDIT_REOPEN", directory.0.join("grants.db"))
        .env(
            "KEEPPEEK_TEST_AUDIT_CLOCK",
            (cutoff + super::super::model::AUDIT_RETENTION_MS).to_string(),
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let output = wait_for_child(child);
    assert!(
        output.status.success(),
        "audit reopen child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let timing = stdout
        .lines()
        .find_map(|line| line.split_once("audit_open_us=").map(|(_, value)| value))
        .expect("audit reopen child must run");
    let duration_us = timing
        .parse::<u128>()
        .expect("child timing in microseconds");
    println!("audit_open_us={duration_us}");

    let connection = raw_connection(&directory);
    pollster::block_on(async {
        let mut rows = connection
            .query("SELECT id FROM guest_grants", ())
            .await
            .unwrap();
        let row = rows
            .next()
            .await
            .unwrap()
            .expect("the newer grant remains in its audit period");
        assert_eq!(row.get::<String>(0).unwrap(), recent.id.to_string());
        assert!(rows.next().await.unwrap().is_none());
    });
}
