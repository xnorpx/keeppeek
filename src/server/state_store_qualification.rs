//! Bounded storage and repeatable control-plane qualification workloads.

use super::*;
use crate::server::state_store::Registry;
use prost_types::{Struct, Value, value::Kind};
use std::path::PathBuf;
use std::time::Instant;

const NOW_MS: u64 = 1_787_000_000_000;
const SCHEMA: &str = "keeppeek.media-intent.v1";

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("keeppeek-qualification-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).expect("qualification directory must be created");
        Self(path)
    }

    fn database(&self) -> PathBuf {
        self.0.join("state.db")
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("qualification directory must be removed");
    }
}

fn value() -> Struct {
    let mut fields = std::collections::BTreeMap::new();
    for (name, text) in [
        ("role", "subscribe".to_owned()),
        ("media_kind", "video".to_owned()),
        ("source_id", "\u{1f4f7}".repeat(128)),
        ("stream_id", "\u{1f4f7}".repeat(128)),
        ("variant_id", "\u{1f4f7}".repeat(128)),
        ("output_profile", "\u{1f4f7}".repeat(128)),
    ] {
        fields.insert(
            name.to_owned(),
            Value {
                kind: Some(Kind::StringValue(text)),
            },
        );
    }
    fields.insert(
        "desired".to_owned(),
        Value {
            kind: Some(Kind::BoolValue(true)),
        },
    );
    Struct { fields }
}

fn put_quota(
    store: &mut DurableStore,
    namespace: &str,
    key: &str,
    value: &Struct,
) -> Result<StoredEntry, Error> {
    store.put(
        namespace,
        key,
        SCHEMA,
        Some(value.clone()),
        Some(0),
        None,
        "admin",
        true,
        NOW_MS,
    )
}

#[test]
fn value_quota_rejects_without_mutation_and_recovers_after_delete() {
    let directory = Directory::new();
    let mut store = DurableStore::open(&directory.database(), NOW_MS).unwrap();
    let value = value();
    let bytes = value.encoded_len() as u64;
    let admitted = MAX_TOTAL_VALUE_BYTES / bytes;
    for index in 0..admitted {
        let namespace = format!("service/quota-{}/", index / 1_024);
        put_quota(&mut store, &namespace, &format!("key-{index}"), &value).unwrap();
    }
    let namespace = format!("service/quota-{}/", admitted / 1_024);
    let before = store.stored_bytes();
    let rejected = put_quota(&mut store, &namespace, "overflow", &value);
    assert_eq!(rejected, Err(Error::Invalid(Invalid::StoreFull)));
    assert_eq!(store.stored_bytes(), before);
    assert_eq!(
        store.get(&namespace, "overflow", "admin", true, NOW_MS),
        Err(Error::NotFound)
    );
    drop(store);
    let mut store = DurableStore::open(&directory.database(), NOW_MS).unwrap();
    assert_eq!(store.stored_bytes(), before);
    let revision = store
        .delete("service/quota-0/", "key-0", Some(1), "admin", true, NOW_MS)
        .unwrap();
    let replacement = put_quota(&mut store, "service/quota-0/", "replacement", &value).unwrap();
    assert_eq!(replacement.revision, revision + 1);
    assert_eq!(store.stored_bytes(), before);
    println!(
        "quota: entries={admitted} value_bytes={bytes} admitted_bytes={before} ceiling={MAX_TOTAL_VALUE_BYTES}"
    );
}

fn report(label: &str, samples_us: &mut [u128]) {
    samples_us.sort_unstable();
    let p50 = samples_us[samples_us.len() / 2];
    let p95 = samples_us[(samples_us.len() * 95).div_ceil(100) - 1];
    let max = samples_us[samples_us.len() - 1];
    println!(
        "{label}: samples={} p50_us={p50} p95_us={p95} max_us={max}",
        samples_us.len()
    );
    assert!(
        max < 5_000_000,
        "a control-plane operation must finish within five seconds"
    );
}

fn resource_sample(system: &mut sysinfo::System) -> (u64, f32) {
    let pid = sysinfo::Pid::from_u32(std::process::id());
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    let process = system
        .process(pid)
        .expect("qualification process must exist");
    (process.memory(), process.cpu_usage())
}

fn idle_resources(system: &mut sysinfo::System) {
    resource_sample(system);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (rss_bytes, cpu_percent) = resource_sample(system);
    println!("idle sample: interval_ms=300 rss_bytes={rss_bytes} cpu_percent={cpu_percent}");
}

#[test]
fn latency_distribution_for_127_principals_over_30_rounds() {
    let directory = Directory::new();
    let mut durable = DurableStore::open(&directory.database(), NOW_MS).unwrap();
    let mut memory = Registry::default();
    let mut baseline_us = Vec::with_capacity(3_810);
    let mut durable_us = Vec::with_capacity(3_810);
    let mut snapshot_us = Vec::with_capacity(3_810);
    let mut system = sysinfo::System::new();
    idle_resources(&mut system);
    let mut sampled_peak_rss_bytes = 0;
    for round in 0..30 {
        for principal in 0..127 {
            let owner = format!("principal-{principal}");
            let namespace = format!("user/{owner}/");
            let start = Instant::now();
            memory
                .put(
                    &namespace,
                    "key",
                    SCHEMA,
                    Some(value()),
                    Some(round),
                    None,
                    &owner,
                    false,
                    NOW_MS,
                )
                .unwrap();
            baseline_us.push(start.elapsed().as_micros());
            let start = Instant::now();
            durable
                .put(
                    &namespace,
                    "key",
                    SCHEMA,
                    Some(value()),
                    Some(round),
                    None,
                    &owner,
                    false,
                    NOW_MS,
                )
                .unwrap();
            durable_us.push(start.elapsed().as_micros());
            let start = Instant::now();
            memory.snapshot(&namespace, "", NOW_MS);
            snapshot_us.push(start.elapsed().as_micros());
        }
        sampled_peak_rss_bytes = sampled_peak_rss_bytes.max(resource_sample(&mut system).0);
    }
    report("memory CAS", &mut baseline_us);
    report("durable CAS", &mut durable_us);
    report("snapshot", &mut snapshot_us);
    println!("resources: sampled_peak_rss_bytes={sampled_peak_rss_bytes} samples=30");
}
