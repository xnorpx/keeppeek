//! Measures the proposed #202 document layout; it does not enable runtime profiles.

use keeppeek::api::proto;
use prost::Message as _;
use prost_types::{ListValue, Struct, Value, value::Kind};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;

const NAMESPACE: &str = "service/runtime-profiles/";
const DOCUMENT_BYTES_MAX: usize = 16 * 1_024;
const SNAPSHOT_BYTES_MAX: usize = 48 * 1_024;
const REVISION: &str = "9223372036854775807";
const PREVIOUS_REVISION: &str = "9223372036854775806";

fn text(value: impl Into<String>) -> Value {
    Value {
        kind: Some(Kind::StringValue(value.into())),
    }
}

fn object(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Struct {
    Struct {
        fields: fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    }
}

const fn nested(value: Struct) -> Value {
    Value {
        kind: Some(Kind::StructValue(value)),
    }
}

fn list(values: impl IntoIterator<Item = Value>) -> Value {
    Value {
        kind: Some(Kind::ListValue(ListValue {
            values: values.into_iter().collect(),
        })),
    }
}

const fn flag() -> Value {
    Value {
        kind: Some(Kind::BoolValue(true)),
    }
}

fn id(index: u32, bytes: usize) -> String {
    let suffix = format!("{index:03}");
    format!("{}{suffix}", "x".repeat(bytes - suffix.len()))
}

fn overrides() -> Struct {
    object([
        ("available", flag()),
        ("recording_mode", text("event-boost")),
        ("privacy", flag()),
        ("notifications_suppressed", flag()),
    ])
}

fn targets(part: u32, observed: bool) -> Value {
    list((part * 16..((part + 1) * 16).min(127)).map(|camera| {
        let mut fields = object([
            ("camera_id", text(id(camera, 256))),
            (
                if observed { "effective" } else { "overrides" },
                nested(overrides()),
            ),
        ]);
        if observed {
            fields.fields.extend(
                object([
                    ("state", text("degraded")),
                    (
                        "provenance",
                        nested(object([
                            ("available", flag()),
                            ("recording_mode", flag()),
                            ("privacy", flag()),
                            ("notifications_suppressed", flag()),
                        ])),
                    ),
                    ("retry_count", text(u32::MAX.to_string())),
                    ("error_code", text("e".repeat(96))),
                ])
                .fields,
            );
        }
        nested(fields)
    }))
}

fn part(index: u32, observed: bool, generation: &str) -> Struct {
    object([
        ("generation", text(generation)),
        ("part", text(index.to_string())),
        ("targets", targets(index, observed)),
    ])
}

fn manifest(profile: u32) -> Struct {
    object([
        ("id", text(id(profile, 128))),
        ("name", text("é".repeat(40))),
        ("generation", text(REVISION)),
        ("expected_revision", text(REVISION)),
        ("target_count", text("127")),
        (
            "part_hashes",
            list((0..8).map(|index| text(part_hash(index)))),
        ),
    ])
}

fn part_hash(index: u32) -> String {
    let mut hash = String::with_capacity(64);
    for byte in Sha256::digest(part(index, false, REVISION).encode_to_vec()) {
        write!(hash, "{byte:02x}").unwrap();
    }
    hash
}

fn registry() -> Struct {
    let profiles = (0..32).map(|profile| {
        nested(object([
            ("id", text(id(profile, 128))),
            ("name", text("é".repeat(40))),
            ("definition_revision", text(REVISION)),
            ("target_count", text("127")),
        ]))
    });
    object([("profiles", list(profiles))])
}

fn entry(key: &str, value: Struct) -> proto::StateEntry {
    proto::StateEntry {
        namespace: NAMESPACE.to_owned(),
        key: key.to_owned(),
        schema: "keeppeek.runtime-profiles.v1".to_owned(),
        value: Some(value),
        revision: i64::MAX.unsigned_abs(),
        updated_at: Some(prost_types::Timestamp {
            seconds: 253_402_300_799,
            nanos: 999_999_999,
        }),
        expires_at: None,
        owner_id: "o".repeat(256),
    }
}

fn snapshot(prefix: &str, entries: Vec<proto::StateEntry>) -> proto::StateWatchSnapshot {
    proto::StateWatchSnapshot {
        watch_id: "w".repeat(128),
        namespace: NAMESPACE.to_owned(),
        key_prefix: prefix.to_owned(),
        snapshot_revision: i64::MAX.unsigned_abs(),
        entries,
    }
}

fn active() -> Struct {
    object([
        ("profile_id", text(id(0, 128))),
        ("definition_generation", text(REVISION)),
        ("desired_generation", text(REVISION)),
        ("observed_parts", text("8")),
    ])
}

const fn response(result: proto::state_store_result::Result) -> proto::ControlEnvelope {
    proto::ControlEnvelope {
        message: Some(proto::control_envelope::Message::Response(
            proto::Response {
                request_id: u64::MAX,
                result: Some(proto::response::Result::Ok(proto::Ok {
                    result: Some(proto::ok::Result::StateStoreResult(
                        proto::StateStoreResult {
                            result: Some(result),
                        },
                    )),
                })),
            },
        )),
    }
}

fn assert_envelope(envelope: proto::ControlEnvelope) -> usize {
    let bytes = envelope.encode_to_vec();
    assert!(bytes.len() < 64 * 1_024);
    assert_eq!(
        proto::ControlEnvelope::decode(bytes.as_slice()).unwrap(),
        envelope
    );
    bytes.len()
}

#[test]
fn full_profile_documents_and_narrow_snapshots_fit_transport_budgets() {
    let documents = [
        registry(),
        manifest(0),
        part(0, false, REVISION),
        part(0, true, REVISION),
        active(),
    ];
    let key = format!("{}/{}", "k".repeat(128), "k".repeat(127));
    for document in documents {
        let document_bytes = document.encode_to_vec();
        assert!(document_bytes.len() <= DOCUMENT_BYTES_MAX);
        assert_eq!(Struct::decode(document_bytes.as_slice()).unwrap(), document);
        let entry = entry(&key, document);
        let get_bytes = assert_envelope(response(proto::state_store_result::Result::Entry(
            entry.clone(),
        )));
        let snapshot = snapshot(&key, vec![entry.clone()]);
        assert!(snapshot.encoded_len() <= SNAPSHOT_BYTES_MAX);
        let watch_bytes =
            assert_envelope(response(proto::state_store_result::Result::Watch(snapshot)));
        assert_put_and_update_fit(&entry);
        println!(
            "document={} get_envelope={} snapshot_envelope={} bytes",
            document_bytes.len(),
            get_bytes,
            watch_bytes
        );
    }
}

fn full_registry() -> Vec<proto::StateEntry> {
    let mut entries = Vec::with_capacity(626);
    for profile in 0..32 {
        entries.push(entry(
            &format!("profiles/{}/manifest", id(profile, 128)),
            manifest(profile),
        ));
        for generation in [PREVIOUS_REVISION, REVISION] {
            for index in 0..8 {
                entries.push(entry(
                    &format!("profiles/{}/{generation}/{index}", id(profile, 128)),
                    part(index, false, generation),
                ));
            }
        }
    }
    for draft in 0..8 {
        entries.push(entry(&format!("drafts/{draft}/manifest"), manifest(draft)));
        for index in 0..8 {
            entries.push(entry(
                &format!("drafts/{draft}/{index}"),
                part(index, false, REVISION),
            ));
        }
    }
    entries.push(entry("registry", registry()));
    entries.push(entry("active", active()));
    entries.extend((0..8).map(|index| {
        entry(
            &format!("observed/{index}/status"),
            part(index, true, REVISION),
        )
    }));
    entries
}

#[test]
fn full_registry_requires_narrow_watches_and_stays_within_proposed_storage_budget() {
    let entries = full_registry();
    assert_eq!(entries.len(), 626);
    let value_bytes: usize = entries
        .iter()
        .map(|entry| entry.value.as_ref().unwrap().encoded_len())
        .sum();
    assert!(value_bytes < 64 * 1_024 * 1_024);
    assert!(
        entries
            .iter()
            .all(|entry| entry.value.as_ref().unwrap().encoded_len() <= DOCUMENT_BYTES_MAX)
    );
    let prefixes: Vec<String> = ["registry".to_owned(), "active".to_owned()]
        .into_iter()
        .chain((0..8).map(|index| format!("observed/{index}/")))
        .collect();
    assert_eq!(prefixes.len(), 10);
    for prefix in &prefixes {
        let matching: Vec<_> = entries
            .iter()
            .filter(|entry| entry.key.starts_with(prefix))
            .cloned()
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "prefix {prefix} must select one document"
        );
        let narrow = snapshot(prefix, matching);
        assert!(narrow.encoded_len() <= SNAPSHOT_BYTES_MAX);
        assert_envelope(response(proto::state_store_result::Result::Watch(narrow)));
    }
    let broad = snapshot("", entries);
    assert!(broad.entries.len() > 64);
    assert!(broad.encoded_len() > 64 * 1_024);
    println!(
        "entries={} values={} broad_snapshot={} bytes",
        broad.entries.len(),
        value_bytes,
        broad.encoded_len()
    );
}

fn assert_put_and_update_fit(entry: &proto::StateEntry) {
    let put = proto::ControlEnvelope {
        message: Some(proto::control_envelope::Message::Request(proto::Request {
            request_id: u64::MAX,
            command: Some(proto::request::Command::StateStoreCommand(
                proto::StateStoreCommand {
                    action: Some(proto::state_store_command::Action::Put(proto::PutState {
                        namespace: entry.namespace.clone(),
                        key: entry.key.clone(),
                        schema: entry.schema.clone(),
                        value: entry.value.clone(),
                        expected_revision: Some(i64::MAX.unsigned_abs()),
                        ttl: None,
                    })),
                },
            )),
        })),
    };
    assert_envelope(put);
    assert_envelope(proto::ControlEnvelope {
        message: Some(proto::control_envelope::Message::Notification(
            proto::Notification {
                event: Some(proto::notification::Event::StateStoreWatchUpdate(
                    proto::StateStoreWatchUpdate {
                        watch_id: "w".repeat(128),
                        namespace: entry.namespace.clone(),
                        key: entry.key.clone(),
                        revision: entry.revision,
                        kind: proto::StateStoreUpdateKind::Put.into(),
                        entry: Some(entry.clone()),
                        watch_sequence: u64::MAX,
                    },
                )),
            },
        )),
    });
}
