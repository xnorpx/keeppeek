//! Actual-process crash/restart coverage for the durable state store.
//!
//! The test re-executes its own test binary as a child process that commits
//! against the production [`super::state_store_durable::DurableStore`]. The
//! parent kills the child mid-commit loop ([`std::process::Child::kill`] runs
//! no destructors and flushes nothing, the portable equivalent of SIGKILL),
//! then a fresh verify child plus an in-process reopen assert that every
//! acknowledged commit survived verbatim, no partial values or counters
//! remain, overdue leases purged on open, and the restored revision serves a
//! fresh snapshot plus compare-and-set.

#[cfg(test)]
mod tests {
    use super::super::state_store::{Registry, StoredEntry};
    use super::super::state_store_durable::DurableStore;
    use prost_types::{Duration, Struct, Value, value::Kind};
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Child, Command};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    const TEST_PATH: &str =
        "server::state_store_crash::tests::durable_kill_restart_preserves_acked_commits";
    const NAMESPACE: &str = "service/transcoder-a/";
    const SCHEMA: &str = "keeppeek.media-intent.v1";
    const OWNER: &str = "transcoder-a";
    const ROUNDS: u32 = 3;
    const COMMITS_PER_ROUND: u32 = 60;
    const KILL_AFTER_ACKS: usize = 20;
    const LEASE_TTL_MS: u64 = 3_600_000;
    const POLL_TIMEOUT_SECS: u64 = 120;

    fn now_ms() -> u64 {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }

    fn media_intent_value(role: &str) -> Struct {
        let mut fields = BTreeMap::from([
            (
                "role".to_owned(),
                Value {
                    kind: Some(Kind::StringValue(role.to_owned())),
                },
            ),
            (
                "source_id".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("front-door".to_owned())),
                },
            ),
            (
                "media_kind".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("video".to_owned())),
                },
            ),
        ]);
        fields.insert(
            "desired".to_owned(),
            Value {
                kind: Some(Kind::BoolValue(true)),
            },
        );
        if role == "publish" {
            fields.insert(
                "recording_mode".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("disabled".to_owned())),
                },
            );
        }
        Struct { fields }
    }

    fn ttl(ms: u64) -> Duration {
        Duration {
            seconds: (ms / 1_000) as i64,
            nanos: ((ms % 1_000) * 1_000_000) as i32,
        }
    }

    fn is_lease(index: u32) -> bool {
        index % 10 == 9
    }

    fn role_for(index: u32) -> &'static str {
        if index.is_multiple_of(2) {
            "publish"
        } else {
            "subscribe"
        }
    }

    struct Ack {
        key: String,
        revision: u64,
        role: String,
        lease: bool,
    }

    fn parse_acks(path: &PathBuf) -> Vec<Ack> {
        let text = std::fs::read_to_string(path).expect("ack log must be readable");
        text.lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                let mut parts = line.splitn(4, ' ');
                Some(Ack {
                    key: parts.next()?.to_owned(),
                    revision: parts.next()?.parse().ok()?,
                    role: parts.next()?.to_owned(),
                    lease: parts.next()? == "lease",
                })
            })
            .collect()
    }

    fn child_write(db: &str, ack_path: &str, round: u32) {
        let mut store = DurableStore::open(std::path::Path::new(db), now_ms())
            .expect("child must open the store");
        let mut acks = std::fs::File::create(ack_path).expect("child must create its ack log");
        for index in 0..COMMITS_PER_ROUND {
            let key = format!("round-{round}-{index}");
            let role = role_for(index);
            let lease = is_lease(index);
            let entry = store
                .put(
                    NAMESPACE,
                    &key,
                    SCHEMA,
                    Some(media_intent_value(role)),
                    None,
                    lease.then(|| ttl(LEASE_TTL_MS)),
                    OWNER,
                    true,
                    now_ms(),
                )
                .expect("child commit must succeed");
            writeln!(
                acks,
                "{} {} {} {}",
                key,
                entry.revision,
                role,
                if lease { "lease" } else { "plain" }
            )
            .expect("child must record its ack");
            acks.sync_all().expect("child ack must reach disk");
        }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }

    fn child_verify(db: &str, dir: &str, now: u64) {
        let mut store = DurableStore::open(std::path::Path::new(db), now)
            .expect("verify child must reopen the store");
        let mut acked = 0;
        let mut purged = 0;
        for round in 0..ROUNDS {
            let path = PathBuf::from(dir).join(format!("acks-{round}.log"));
            if !path.exists() {
                continue;
            }
            for ack in parse_acks(&path) {
                if ack.lease {
                    assert!(
                        store.get(NAMESPACE, &ack.key, OWNER, true, now).is_err(),
                        "overdue lease {} must purge on open",
                        ack.key
                    );
                    purged += 1;
                    continue;
                }
                let entry = store
                    .get(NAMESPACE, &ack.key, OWNER, true, now)
                    .expect("acked commit must survive the kill");
                assert_eq!(entry.revision, ack.revision);
                assert_eq!(entry.value, media_intent_value(&ack.role));
                acked += 1;
            }
        }
        let before = store
            .export(now)
            .expect("verify child must export a coherent store");
        let export_revision = before
            .iter()
            .find(|export| export.namespace == NAMESPACE)
            .expect("verify child must see the namespace")
            .revision;
        let resumed = store
            .put(
                NAMESPACE,
                "round-resume",
                SCHEMA,
                Some(media_intent_value("publish")),
                None,
                None,
                OWNER,
                true,
                now,
            )
            .expect("writes must resume after the kill");
        assert_eq!(
            resumed.revision,
            export_revision + 1,
            "the revision chain must continue without reuse"
        );
        println!(
            "CRASH-VERIFY-OK acked={acked} purged={purged} resumed_rev={}",
            resumed.revision
        );
    }

    fn spawn_child(mode: &str, db: &str, extra: &str, round: &str) -> Child {
        let exe = std::env::current_exe().expect("test binary path must resolve");
        Command::new(exe)
            .args(["--nocapture", "--exact", TEST_PATH])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env("KEEPPEEK_CRASH_MODE", mode)
            .env("KEEPPEEK_CRASH_DB", db)
            .env("KEEPPEEK_CRASH_EXTRA", extra)
            .env("KEEPPEEK_CRASH_ROUND", round)
            .spawn()
            .expect("crash child must spawn")
    }

    fn wait_for_acks(ack_path: &PathBuf, round: u32) {
        let deadline = Instant::now() + std::time::Duration::from_secs(POLL_TIMEOUT_SECS);
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(ack_path)
                && text.lines().filter(|line| !line.is_empty()).count() >= KILL_AFTER_ACKS + 2
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("round {round} never reached {KILL_AFTER_ACKS} acks before the kill deadline");
    }

    #[test]
    fn durable_kill_restart_preserves_acked_commits() {
        if let Ok(mode) = std::env::var("KEEPPEEK_CRASH_MODE") {
            let db = std::env::var("KEEPPEEK_CRASH_DB").expect("child needs a db path");
            let extra = std::env::var("KEEPPEEK_CRASH_EXTRA").expect("child needs its extra arg");
            if mode == "verify" {
                let now: u64 = extra.parse().expect("verify needs a clock");
                child_verify(
                    &db,
                    &std::env::var("KEEPPEEK_CRASH_ROUND").expect("verify needs a dir"),
                    now,
                );
            } else {
                let round: u32 = std::env::var("KEEPPEEK_CRASH_ROUND")
                    .expect("writer needs a round")
                    .parse()
                    .expect("round must parse");
                child_write(&db, &extra, round);
            }
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "keeppeek-crash-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("crash scratch dir must build");
        let db = dir.join("state-store.db");
        let db_text = db.to_str().expect("scratch path must be UTF-8").to_owned();
        for round in 0..ROUNDS {
            let ack_path = dir.join(format!("acks-{round}.log"));
            let mut child = spawn_child(
                "write",
                &db_text,
                ack_path.to_str().expect("ack path must be UTF-8"),
                &round.to_string(),
            );
            wait_for_acks(&ack_path, round);
            let _ = child.kill();
            let _ = child.wait();
        }
        let mut acked: Vec<Ack> = Vec::new();
        for round in 0..ROUNDS {
            acked.extend(parse_acks(&dir.join(format!("acks-{round}.log"))));
        }
        assert!(
            acked.len() >= ROUNDS as usize * KILL_AFTER_ACKS,
            "every round must ack before its kill"
        );
        let max_acked = acked.iter().map(|ack| ack.revision).max().unwrap_or(0);
        let export = {
            let reopened = DurableStore::open(&db, now_ms()).expect("killed store must reopen");
            reopened
                .export(now_ms())
                .expect("export must decode every row")
        };
        let namespace = export
            .iter()
            .find(|namespace| namespace.namespace == NAMESPACE)
            .expect("reopen must restore the namespace");
        assert!(
            namespace.revision >= max_acked && namespace.revision <= max_acked + u64::from(ROUNDS),
            "the counter must cover every ack with room for at most one unacked commit per round"
        );
        let acked_keys: BTreeSet<&str> = acked.iter().map(|ack| ack.key.as_str()).collect();
        let present_keys: BTreeSet<&str> = namespace
            .entries
            .iter()
            .map(|entry| entry.key.as_str())
            .collect();
        let extras: Vec<&&str> = present_keys.difference(&acked_keys).collect();
        assert!(
            extras.len() <= ROUNDS as usize,
            "at most one unacked commit per round may have slipped in: {extras:?}"
        );
        for ack in &acked {
            if ack.lease {
                continue;
            }
            let entry: StoredEntry = namespace
                .entries
                .iter()
                .find(|entry| entry.key == ack.key)
                .expect("every acked commit must be present")
                .clone();
            assert_eq!(entry.revision, ack.revision);
            assert_eq!(entry.value, media_intent_value(&ack.role));
        }
        assert_eq!(
            namespace
                .entries
                .iter()
                .filter(|entry| entry.expires_ms.is_some())
                .count(),
            acked.iter().filter(|ack| ack.lease).count(),
            "leases killed before expiry must recover intact"
        );
        let mut registry = Registry::default();
        registry.import_namespace(
            NAMESPACE.to_owned(),
            namespace.revision,
            namespace.entries.clone(),
        );
        let (snapshot_revision, snapshot_entries) = registry.snapshot(NAMESPACE, "", now_ms());
        assert_eq!(snapshot_revision, namespace.revision);
        assert_eq!(snapshot_entries.len(), namespace.entries.len());
        let restored = snapshot_entries
            .iter()
            .find(|entry| entry.key == "round-0-0")
            .expect("first acked key must serve from the restored revision");
        registry
            .put(
                NAMESPACE,
                "round-0-0",
                SCHEMA,
                Some(media_intent_value("subscribe")),
                Some(restored.revision),
                None,
                OWNER,
                true,
                now_ms(),
            )
            .expect("CAS against the restored revision must succeed");
        registry
            .put(
                NAMESPACE,
                "round-0-0",
                SCHEMA,
                Some(media_intent_value("publish")),
                Some(restored.revision),
                None,
                OWNER,
                true,
                now_ms(),
            )
            .expect_err("a stale CAS against the restored revision must conflict");
        let verify_now = now_ms().saturating_add(2 * LEASE_TTL_MS);
        let dir_text = dir.to_str().expect("scratch path must be UTF-8").to_owned();
        let verify = spawn_child("verify", &db_text, &verify_now.to_string(), &dir_text)
            .wait_with_output()
            .expect("verify child must run");
        assert!(
            verify.status.success(),
            "verify child must exit clean: {}",
            String::from_utf8_lossy(&verify.stderr)
        );
        let stdout = String::from_utf8_lossy(&verify.stdout);
        assert!(
            stdout.contains("CRASH-VERIFY-OK"),
            "verify child must report success: {stdout}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
