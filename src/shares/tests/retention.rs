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

    let mut fixture = Fixture::new();
    seed_history(
        &fixture,
        Seed {
            count: 257,
            terminal: true,
            group_creators: false,
            created_ms: fixture.now.unix_ms,
        },
    );
    let (recent, _) = fixture.grant();
    let Fixture {
        store,
        directory,
        now,
        ..
    } = fixture;
    drop(store);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env("KEEPPEEK_TEST_AUDIT_REOPEN", directory.0.join("grants.db"))
        .env(
            "KEEPPEEK_TEST_AUDIT_CLOCK",
            (now.unix_ms + super::super::model::AUDIT_RETENTION_MS).to_string(),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "audit reopen child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let timing = stdout
        .lines()
        .find(|line| line.starts_with("audit_open_us="))
        .expect("audit reopen child must run");
    println!("{timing}");

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
