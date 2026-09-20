//! Settings-backed state-store namespace adapter (*#39* slice 2b).
//!
//! A registered adapter namespace stores its documents and namespace revision
//! together in one `config.toml` section. The file is the sole durable
//! authority: the in-memory registry and the runtime database never duplicate
//! adapter documents or counters.
//!
//! Lock rule: adapter commands hold the shared `config_update` lock and never
//! the registry lock, while generic registry commands hold the registry lock
//! and never `config_update`. The two paths are disjoint, so no lock order
//! can invert and no live parallel writer can interleave a section update.
//! Every mutation reloads the current table under that lock, validates the
//! complete candidate, and replaces the file with the existing atomic writer,
//! so a crash leaves the previous complete section or the new complete
//! section behind, never a mix.
//!
//! Adapter entries are configuration, not leases: TTL requests are rejected
//! and entries never expire.

use super::state_store::{
    Error, Invalid, MAX_ENTRIES_PER_NAMESPACE, StoredEntry, authorize_read, authorize_write,
    check_expected, validate_key, validate_namespace,
};
use prost_types::{Duration, Struct, Value, value::Kind};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

pub(super) const SETTINGS_TEST_NAMESPACE: &str = "service/state-store-test/";
pub(super) const SETTINGS_TEST_SCHEMA: &str = "keeppeek.settings-test.v1";
const ADAPTER_SECTION: &str = "state_store";
const MAX_DISPLAY_NAME_CHARS: usize = 128;

pub(super) fn is_adapter_namespace(namespace: &str) -> bool {
    namespace == SETTINGS_TEST_NAMESPACE
}

pub(super) fn get(
    config_lock: &Mutex<()>,
    config_path: &Path,
    namespace: &str,
    key: &str,
    owner_id: &str,
    reader_admin: bool,
) -> Result<StoredEntry, Error> {
    let layout = validate_namespace(namespace)?;
    authorize_read(&layout, owner_id, reader_admin)?;
    validate_key(key)?;
    let _guard = lock_config(config_lock)?;
    let root = load_table(config_path)?;
    let section = load_section(&root, namespace)?;
    let entry = section.entries.get(key).ok_or(Error::NotFound)?;
    Ok(stored_entry(namespace, key, entry))
}

#[allow(
    clippy::too_many_arguments,
    reason = "adapter put mirrors the validated registry inputs one-to-one"
)]
pub(super) fn put(
    config_lock: &Mutex<()>,
    config_path: &Path,
    namespace: &str,
    key: &str,
    schema: &str,
    value: Option<Struct>,
    expected_revision: Option<u64>,
    ttl: Option<Duration>,
    owner_id: &str,
    writer_admin: bool,
    now_ms: u64,
) -> Result<StoredEntry, Error> {
    let layout = validate_namespace(namespace)?;
    validate_key(key)?;
    if schema != SETTINGS_TEST_SCHEMA {
        return Err(Error::Invalid(Invalid::Schema));
    }
    let value = value.ok_or(Error::Invalid(Invalid::ValueMissing))?;
    if ttl.is_some() {
        return Err(Error::Invalid(Invalid::Ttl));
    }
    let profile = TestProfile::from_struct(&value)?;
    authorize_write(&layout, owner_id, writer_admin)?;
    let _guard = lock_config(config_lock)?;
    let mut root = load_table(config_path)?;
    let mut section = load_section(&root, namespace)?;
    let current = section.entries.get(key).map(|entry| entry.revision);
    check_expected(expected_revision, current)?;
    if current.is_none() && section.entries.len() >= MAX_ENTRIES_PER_NAMESPACE {
        return Err(Error::Invalid(Invalid::NamespaceFull));
    }
    let revision = section.bump_revision()?;
    section.entries.insert(
        key.to_owned(),
        SectionEntry {
            revision,
            schema: schema.to_owned(),
            owner_id: owner_id.to_owned(),
            updated_ms: now_ms,
            profile,
        },
    );
    store_section(&mut root, namespace, &section)?;
    write_table(config_path, &root)?;
    let entry = section
        .entries
        .get(key)
        .expect("adapter entry must exist after insert");
    Ok(stored_entry(namespace, key, entry))
}

pub(super) fn delete(
    config_lock: &Mutex<()>,
    config_path: &Path,
    namespace: &str,
    key: &str,
    expected_revision: Option<u64>,
    owner_id: &str,
    writer_admin: bool,
) -> Result<u64, Error> {
    let layout = validate_namespace(namespace)?;
    validate_key(key)?;
    authorize_write(&layout, owner_id, writer_admin)?;
    let _guard = lock_config(config_lock)?;
    let mut root = load_table(config_path)?;
    let mut section = load_section(&root, namespace)?;
    let current = section.entries.get(key).map(|entry| entry.revision);
    check_expected(expected_revision, current)?;
    if current.is_none() {
        return Err(Error::NotFound);
    }
    let revision = section.bump_revision()?;
    section.entries.remove(key);
    store_section(&mut root, namespace, &section)?;
    write_table(config_path, &root)?;
    Ok(revision)
}

#[derive(Clone, Debug, PartialEq)]
struct TestProfile {
    display_name: String,
    enabled: bool,
}

impl TestProfile {
    fn from_struct(value: &Struct) -> Result<Self, Error> {
        let display_name = match value
            .fields
            .get("display_name")
            .and_then(|v| v.kind.as_ref())
        {
            Some(Kind::StringValue(name)) => name.clone(),
            _ => return Err(Error::Invalid(Invalid::Schema)),
        };
        if display_name.chars().count() > MAX_DISPLAY_NAME_CHARS {
            return Err(Error::Invalid(Invalid::Schema));
        }
        let enabled = match value.fields.get("enabled").and_then(|v| v.kind.as_ref()) {
            Some(Kind::BoolValue(enabled)) => *enabled,
            _ => return Err(Error::Invalid(Invalid::Schema)),
        };
        if value.fields.len() != 2 {
            return Err(Error::Invalid(Invalid::Schema));
        }
        Ok(Self {
            display_name,
            enabled,
        })
    }

    fn to_struct(&self) -> Struct {
        Struct {
            fields: BTreeMap::from([
                (
                    "display_name".to_owned(),
                    Value {
                        kind: Some(Kind::StringValue(self.display_name.clone())),
                    },
                ),
                (
                    "enabled".to_owned(),
                    Value {
                        kind: Some(Kind::BoolValue(self.enabled)),
                    },
                ),
            ]),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct SectionEntry {
    revision: u64,
    schema: String,
    owner_id: String,
    updated_ms: u64,
    profile: TestProfile,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Section {
    revision: u64,
    entries: BTreeMap<String, SectionEntry>,
}

impl Section {
    fn bump_revision(&mut self) -> Result<u64, Error> {
        let next = self.revision.checked_add(1).ok_or_else(|| {
            Error::Storage("state-store settings revision overflows u64".to_owned())
        })?;
        if next > i64::MAX as u64 {
            return Err(Error::Storage(
                "state-store settings revision exceeds the persisted range".to_owned(),
            ));
        }
        self.revision = next;
        Ok(self.revision)
    }
}

fn lock_config(lock: &Mutex<()>) -> Result<std::sync::MutexGuard<'_, ()>, Error> {
    lock.lock()
        .map_err(|_| Error::Storage("state-store settings lock is unavailable".to_owned()))
}

fn load_table(path: &Path) -> Result<toml::Table, Error> {
    crate::config::load_configuration_table(path)
        .map_err(|error| Error::Storage(format!("state-store settings load failed: {error}")))
}

fn write_table(path: &Path, root: &toml::Table) -> Result<(), Error> {
    crate::config::write_configuration_table(path, root)
        .map_err(|error| Error::Storage(format!("state-store settings write failed: {error}")))
}

fn load_section(root: &toml::Table, namespace: &str) -> Result<Section, Error> {
    let Some(adapters) = root.get(ADAPTER_SECTION) else {
        return Ok(Section::default());
    };
    let adapters = adapters
        .as_table()
        .ok_or_else(|| Error::Storage("state-store settings section is corrupt".to_owned()))?;
    let Some(section) = adapters.get(namespace) else {
        return Ok(Section::default());
    };
    let table = section
        .as_table()
        .ok_or_else(|| Error::Storage("state-store settings section is corrupt".to_owned()))?;
    let mut entries = BTreeMap::new();
    if let Some(tables) = table.get("entries") {
        let tables = tables
            .as_table()
            .ok_or_else(|| Error::Storage("state-store settings entries are corrupt".to_owned()))?;
        for (key, value) in tables {
            entries.insert(key.clone(), load_entry(key, value)?);
        }
    }
    let revision = match table.get("revision") {
        None if entries.is_empty() => 0,
        None => {
            return Err(Error::Storage(
                "state-store settings revision is missing".to_owned(),
            ));
        }
        Some(value) => value
            .as_integer()
            .filter(|revision| *revision >= 0)
            .map(|revision| revision as u64)
            .ok_or_else(|| Error::Storage("state-store settings revision is corrupt".to_owned()))?,
    };
    for (key, entry) in &entries {
        if entry.revision == 0 || entry.revision > revision {
            return Err(Error::Storage(format!(
                "state-store settings entry {key} revision is inconsistent"
            )));
        }
    }
    Ok(Section { revision, entries })
}

fn load_entry(key: &str, value: &toml::Value) -> Result<SectionEntry, Error> {
    let corrupt = || Error::Storage(format!("state-store settings entry {key} is corrupt"));
    let table = value.as_table().ok_or_else(corrupt)?;
    let integer = |name: &str| {
        table
            .get(name)
            .and_then(toml::Value::as_integer)
            .filter(|value| *value >= 0)
            .map(|value| value as u64)
            .ok_or_else(corrupt)
    };
    let text = |name: &str| {
        table
            .get(name)
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(corrupt)
    };
    let revision = integer("revision")?;
    let schema = text("schema")?;
    if schema != SETTINGS_TEST_SCHEMA {
        return Err(corrupt());
    }
    let owner_id = text("owner_id")?;
    let updated_ms = integer("updated_ms")?;
    let display_name = text("display_name")?;
    if display_name.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err(corrupt());
    }
    let enabled = table
        .get("enabled")
        .and_then(toml::Value::as_bool)
        .ok_or_else(corrupt)?;
    Ok(SectionEntry {
        revision,
        schema,
        owner_id,
        updated_ms,
        profile: TestProfile {
            display_name,
            enabled,
        },
    })
}

fn persisted_integer(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| {
        Error::Storage("state-store settings integer exceeds the persisted range".to_owned())
    })
}

fn store_section(root: &mut toml::Table, namespace: &str, section: &Section) -> Result<(), Error> {
    let mut entries = toml::Table::new();
    for (key, entry) in &section.entries {
        let mut table = toml::Table::new();
        table.insert(
            "revision".to_owned(),
            toml::Value::Integer(persisted_integer(entry.revision)?),
        );
        table.insert(
            "schema".to_owned(),
            toml::Value::String(entry.schema.clone()),
        );
        table.insert(
            "owner_id".to_owned(),
            toml::Value::String(entry.owner_id.clone()),
        );
        table.insert(
            "updated_ms".to_owned(),
            toml::Value::Integer(persisted_integer(entry.updated_ms)?),
        );
        table.insert(
            "display_name".to_owned(),
            toml::Value::String(entry.profile.display_name.clone()),
        );
        table.insert(
            "enabled".to_owned(),
            toml::Value::Boolean(entry.profile.enabled),
        );
        entries.insert(key.clone(), toml::Value::Table(table));
    }
    let mut table = toml::Table::new();
    table.insert(
        "revision".to_owned(),
        toml::Value::Integer(persisted_integer(section.revision)?),
    );
    table.insert("entries".to_owned(), toml::Value::Table(entries));
    let Some(adapters) = root
        .entry(ADAPTER_SECTION.to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
    else {
        return Err(Error::Storage(
            "state-store settings section cannot hold the namespace".to_owned(),
        ));
    };
    adapters.insert(namespace.to_owned(), toml::Value::Table(table));
    Ok(())
}

fn stored_entry(namespace: &str, key: &str, entry: &SectionEntry) -> StoredEntry {
    StoredEntry {
        namespace: namespace.to_owned(),
        key: key.to_owned(),
        schema: entry.schema.clone(),
        value: entry.profile.to_struct(),
        revision: entry.revision,
        updated_ms: entry.updated_ms,
        expires_ms: None,
        owner_id: entry.owner_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::proto::{self, ok as control_ok, state_store_command, state_store_result};
    use crate::config::Config;
    use crate::server::{ApiPrincipal, ServerState};
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR_ID: AtomicU64 = AtomicU64::new(0);

    fn seed_config(path: &Path) {
        let seed =
            toml::to_string_pretty(&Config::default()).expect("default config must serialize");
        std::fs::write(path, seed).expect("test config must be created");
    }

    struct Fixture {
        dir: PathBuf,
        lock: Mutex<()>,
    }

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "keeppeek-state-store-settings-{}-{}",
                std::process::id(),
                id
            ));
            std::fs::create_dir_all(&dir).expect("test temp dir must be created");
            seed_config(&dir.join("config.toml"));
            Self {
                dir,
                lock: Mutex::new(()),
            }
        }

        fn config_path(&self) -> PathBuf {
            self.dir.join("config.toml")
        }

        fn file_bytes(&self) -> Vec<u8> {
            std::fs::read(self.config_path()).expect("test config must be readable")
        }

        fn put(
            &self,
            key: &str,
            display_name: &str,
            enabled: bool,
            expected_revision: Option<u64>,
            now_ms: u64,
        ) -> Result<StoredEntry, Error> {
            put(
                &self.lock,
                &self.config_path(),
                SETTINGS_TEST_NAMESPACE,
                key,
                SETTINGS_TEST_SCHEMA,
                Some(profile_struct(display_name, enabled)),
                expected_revision,
                None,
                "local-administrator",
                true,
                now_ms,
            )
        }

        fn get(&self, key: &str) -> Result<StoredEntry, Error> {
            get(
                &self.lock,
                &self.config_path(),
                SETTINGS_TEST_NAMESPACE,
                key,
                "local-administrator",
                true,
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn profile_struct(display_name: &str, enabled: bool) -> Struct {
        Struct {
            fields: BTreeMap::from([
                (
                    "display_name".to_owned(),
                    Value {
                        kind: Some(Kind::StringValue(display_name.to_owned())),
                    },
                ),
                (
                    "enabled".to_owned(),
                    Value {
                        kind: Some(Kind::BoolValue(enabled)),
                    },
                ),
            ]),
        }
    }

    const NOW_MS: u64 = 1_787_000_000_000;

    #[test]
    fn settings_section_is_reserved_from_camera_parsing() {
        assert!(crate::config::is_reserved_section("state_store"));
    }

    fn rewrite_config(fixture: &Fixture, mutate: impl FnOnce(&mut toml::Table)) {
        let mut root: toml::Table = std::fs::read_to_string(fixture.config_path())
            .expect("config must be readable")
            .parse()
            .expect("config must parse");
        mutate(&mut root);
        std::fs::write(
            fixture.config_path(),
            toml::to_string_pretty(&root).expect("config must serialize"),
        )
        .expect("config must be writable");
    }

    fn adapter_table(root: &mut toml::Table) -> &mut toml::Table {
        root.get_mut(ADAPTER_SECTION)
            .and_then(|value| value.as_table_mut())
            .and_then(|adapters| adapters.get_mut(SETTINGS_TEST_NAMESPACE))
            .and_then(|value| value.as_table_mut())
            .expect("adapter section must exist")
    }

    #[test]
    fn settings_scalar_root_fails_without_touching_file() {
        let fixture = Fixture::new();
        let seed = std::fs::read_to_string(fixture.config_path()).expect("config must exist");
        std::fs::write(fixture.config_path(), format!("state_store = 42\n{seed}"))
            .expect("scalar root must be seeded");
        let before = fixture.file_bytes();
        let put_error = fixture
            .put("alerts/front-door", "Front door", true, None, NOW_MS)
            .expect_err("scalar root must fail");
        assert!(
            matches!(put_error, Error::Storage(_)),
            "unexpected error: {put_error:?}"
        );
        assert_eq!(
            fixture.file_bytes(),
            before,
            "failed write must leave the file untouched"
        );
        let get_error = fixture
            .get("alerts/front-door")
            .expect_err("scalar root must not read as absent");
        assert!(
            matches!(get_error, Error::Storage(_)),
            "unexpected error: {get_error:?}"
        );
    }

    #[test]
    fn settings_inconsistent_counters_are_rejected() {
        for mutate in [
            |table: &mut toml::Table| {
                table.remove("revision");
            },
            |table: &mut toml::Table| {
                table.insert("revision".to_owned(), toml::Value::Integer(0));
            },
            |table: &mut toml::Table| {
                let entries = table
                    .get_mut("entries")
                    .and_then(|value| value.as_table_mut())
                    .expect("entries must exist");
                let entry = entries
                    .get_mut("alerts/front-door")
                    .and_then(|value| value.as_table_mut())
                    .expect("entry must exist");
                entry.insert("revision".to_owned(), toml::Value::Integer(99));
            },
        ] {
            let fixture = Fixture::new();
            fixture
                .put("alerts/front-door", "Front door", true, None, NOW_MS)
                .expect("seed write must succeed");
            rewrite_config(&fixture, |root| mutate(adapter_table(root)));
            let before = fixture.file_bytes();
            let get_error = fixture
                .get("alerts/front-door")
                .expect_err("inconsistent counter must fail");
            assert!(
                matches!(get_error, Error::Storage(_)),
                "unexpected error: {get_error:?}"
            );
            let put_error = fixture
                .put("alerts/front-door", "Front door", true, Some(1), NOW_MS)
                .expect_err("CAS against inconsistent state must fail");
            assert!(
                matches!(put_error, Error::Storage(_)),
                "unexpected error: {put_error:?}"
            );
            assert_eq!(
                fixture.file_bytes(),
                before,
                "rejected state must leave the file unchanged"
            );
        }
    }

    #[test]
    fn settings_mutations_reload_with_exact_revision() {
        let fixture = Fixture::new();
        let first = fixture
            .put("alerts/front-door", "Front door", true, None, NOW_MS)
            .expect("put must succeed");
        let second = fixture
            .put("alerts/back-door", "Back door", false, None, NOW_MS)
            .expect("put must succeed");
        let deleted = delete(
            &fixture.lock,
            &fixture.config_path(),
            SETTINGS_TEST_NAMESPACE,
            "alerts/back-door",
            None,
            "local-administrator",
            true,
        )
        .expect("delete must succeed");
        let root: toml::Table = std::fs::read_to_string(fixture.config_path())
            .expect("config must be readable")
            .parse()
            .expect("config must parse");
        let section =
            load_section(&root, SETTINGS_TEST_NAMESPACE).expect("section must reload cleanly");
        assert_eq!(section.revision, deleted);
        assert_eq!(
            section
                .entries
                .get("alerts/front-door")
                .map(|entry| entry.revision),
            Some(first.revision)
        );
        assert_eq!(
            section.entries.get("alerts/back-door"),
            None,
            "deleted entry must stay deleted after reload"
        );
        assert_eq!(second.revision + 1, deleted);
    }

    #[test]
    fn settings_persisted_range_overflow_is_rejected() {
        let mut section = Section {
            revision: i64::MAX as u64,
            entries: BTreeMap::new(),
        };
        assert!(
            section.bump_revision().is_err(),
            "bump past the persisted range must fail"
        );
        assert_eq!(section.revision, i64::MAX as u64);
        let overflow = Section {
            revision: i64::MAX as u64 + 1,
            entries: BTreeMap::new(),
        };
        assert!(
            store_section(&mut toml::Table::new(), SETTINGS_TEST_NAMESPACE, &overflow).is_err(),
            "storing beyond the persisted range must fail"
        );
        assert!(persisted_integer(u64::MAX).is_err());
    }

    #[test]
    fn settings_put_get_roundtrip() {
        let fixture = Fixture::new();
        let entry = fixture
            .put("alerts/front-door", "Front door", true, None, NOW_MS)
            .expect("put must succeed");
        assert_eq!(entry.revision, 1);
        assert_eq!(entry.schema, SETTINGS_TEST_SCHEMA);
        assert_eq!(entry.expires_ms, None);
        let read = fixture.get("alerts/front-door").expect("get must succeed");
        assert_eq!(read, entry);
    }

    #[test]
    fn settings_competing_cas_writers_have_exactly_one_winner() {
        let fixture = Fixture::new();
        fixture
            .put("alerts/front-door", "Front door", true, None, NOW_MS)
            .expect("seed write must succeed");
        let winner = fixture
            .put("alerts/front-door", "Front door v2", true, Some(1), NOW_MS)
            .expect("matching compare-and-set must succeed");
        assert_eq!(winner.revision, 2);
        let error = fixture
            .put("alerts/front-door", "Front door v3", false, Some(1), NOW_MS)
            .expect_err("stale compare-and-set must fail");
        assert_eq!(
            error,
            Error::Conflict {
                current_revision: 2
            }
        );
        let read = fixture.get("alerts/front-door").expect("get must succeed");
        assert_eq!(read.revision, 2);
        assert_eq!(
            read.value,
            profile_struct("Front door v2", true),
            "loser must not overwrite the winner"
        );
    }

    #[test]
    fn settings_create_only_delete_and_missing_paths() {
        let fixture = Fixture::new();
        fixture
            .put("alerts/front-door", "Front door", true, Some(0), NOW_MS)
            .expect("create-only write must succeed");
        let error = fixture
            .put("alerts/front-door", "Front door", true, Some(0), NOW_MS)
            .expect_err("second create-only write must fail");
        assert_eq!(
            error,
            Error::Conflict {
                current_revision: 1
            }
        );
        let revision = delete(
            &fixture.lock,
            &fixture.config_path(),
            SETTINGS_TEST_NAMESPACE,
            "alerts/front-door",
            None,
            "local-administrator",
            true,
        )
        .expect("delete must succeed");
        assert_eq!(revision, 2);
        let error = delete(
            &fixture.lock,
            &fixture.config_path(),
            SETTINGS_TEST_NAMESPACE,
            "alerts/front-door",
            None,
            "local-administrator",
            true,
        )
        .expect_err("second delete must fail");
        assert_eq!(error, Error::NotFound);
        let error = fixture
            .get("alerts/front-door")
            .expect_err("deleted entries must read as not found");
        assert_eq!(error, Error::NotFound);
    }

    #[test]
    fn settings_rejects_unknown_schema_bad_profiles_and_ttl() {
        let fixture = Fixture::new();
        let before = fixture.file_bytes();
        let bad_schema = put(
            &fixture.lock,
            &fixture.config_path(),
            SETTINGS_TEST_NAMESPACE,
            "alerts/front-door",
            "keeppeek.unknown.v1",
            Some(profile_struct("Front door", true)),
            None,
            None,
            "local-administrator",
            true,
            NOW_MS,
        )
        .expect_err("unknown schema must fail");
        assert_eq!(bad_schema, Error::Invalid(Invalid::Schema));
        let missing_field = put(
            &fixture.lock,
            &fixture.config_path(),
            SETTINGS_TEST_NAMESPACE,
            "alerts/front-door",
            SETTINGS_TEST_SCHEMA,
            Some(Struct {
                fields: BTreeMap::from([(
                    "display_name".to_owned(),
                    Value {
                        kind: Some(Kind::StringValue("Front door".to_owned())),
                    },
                )]),
            }),
            None,
            None,
            "local-administrator",
            true,
            NOW_MS,
        )
        .expect_err("missing field must fail");
        assert_eq!(missing_field, Error::Invalid(Invalid::Schema));
        let mut extra = profile_struct("Front door", true);
        extra.fields.insert(
            "password".to_owned(),
            Value {
                kind: Some(Kind::StringValue("secret".to_owned())),
            },
        );
        let extra_field = put(
            &fixture.lock,
            &fixture.config_path(),
            SETTINGS_TEST_NAMESPACE,
            "alerts/front-door",
            SETTINGS_TEST_SCHEMA,
            Some(extra),
            None,
            None,
            "local-administrator",
            true,
            NOW_MS,
        )
        .expect_err("undeclared field must fail");
        assert_eq!(extra_field, Error::Invalid(Invalid::Schema));
        let ttl = put(
            &fixture.lock,
            &fixture.config_path(),
            SETTINGS_TEST_NAMESPACE,
            "alerts/front-door",
            SETTINGS_TEST_SCHEMA,
            Some(profile_struct("Front door", true)),
            None,
            Some(Duration {
                seconds: 60,
                nanos: 0,
            }),
            "local-administrator",
            true,
            NOW_MS,
        )
        .expect_err("TTL on settings entries must fail");
        assert_eq!(ttl, Error::Invalid(Invalid::Ttl));
        assert_eq!(
            fixture.file_bytes(),
            before,
            "failed validation must leave the file untouched"
        );
        assert_eq!(
            fixture
                .get("alerts/front-door")
                .expect_err("nothing must have been stored"),
            Error::NotFound
        );
    }

    #[test]
    fn settings_service_writes_require_administrator() {
        let fixture = Fixture::new();
        let error = put(
            &fixture.lock,
            &fixture.config_path(),
            SETTINGS_TEST_NAMESPACE,
            "alerts/front-door",
            SETTINGS_TEST_SCHEMA,
            Some(profile_struct("Front door", true)),
            None,
            None,
            "viewer-a",
            false,
            NOW_MS,
        )
        .expect_err("non-admin service write must fail");
        assert_eq!(error, Error::NotAuthorized);
    }

    #[test]
    fn settings_restart_preserves_entries_and_revision() {
        let fixture = Fixture::new();
        fixture
            .put("alerts/front-door", "Front door", true, None, NOW_MS)
            .expect("put must succeed");
        fixture
            .put("alerts/back-door", "Back door", false, None, NOW_MS)
            .expect("put must succeed");
        delete(
            &fixture.lock,
            &fixture.config_path(),
            SETTINGS_TEST_NAMESPACE,
            "alerts/back-door",
            None,
            "local-administrator",
            true,
        )
        .expect("delete must succeed");
        let read = fixture
            .get("alerts/front-door")
            .expect("entry must survive");
        assert_eq!(read.revision, 1);
        let next = fixture
            .put("alerts/garage", "Garage", true, None, NOW_MS)
            .expect("post-restart write must succeed");
        assert_eq!(
            next.revision, 4,
            "revision must stay monotonic across empty deletes"
        );
    }

    #[test]
    fn settings_mutation_preserves_unrelated_sections() {
        let fixture = Fixture::new();
        let mut root: toml::Table = std::fs::read_to_string(fixture.config_path())
            .expect("config must be readable")
            .parse()
            .expect("config must parse");
        let mut logging = toml::Table::new();
        logging.insert("level".to_owned(), toml::Value::String("debug".to_owned()));
        root.insert("logging".to_owned(), toml::Value::Table(logging));
        std::fs::write(
            fixture.config_path(),
            toml::to_string_pretty(&root).expect("config must serialize"),
        )
        .expect("unrelated section must be seeded");
        fixture
            .put("alerts/front-door", "Front door", true, None, NOW_MS)
            .expect("put must succeed");
        let root: toml::Table = std::fs::read_to_string(fixture.config_path())
            .expect("config must be readable")
            .parse()
            .expect("config must stay valid TOML");
        assert_eq!(
            root.get("logging")
                .and_then(|value| value.get("level"))
                .and_then(toml::Value::as_str),
            Some("debug"),
            "unrelated sections must survive adapter writes"
        );
    }

    #[test]
    fn settings_dispatch_routes_adapter_namespace_to_config() {
        let dir = std::env::temp_dir().join(format!(
            "keeppeek-state-store-settings-dispatch-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("test temp dir must be created");
        let config_path = dir.join("config.toml");
        seed_config(&config_path);
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let principal = ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let command = proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: SETTINGS_TEST_NAMESPACE.to_owned(),
                key: "alerts/front-door".to_owned(),
                schema: SETTINGS_TEST_SCHEMA.to_owned(),
                value: Some(profile_struct("Front door", true)),
                expected_revision: None,
                ttl: None,
            })),
        };
        let control_ok::Result::StateStoreResult(result) =
            super::super::state_store::dispatch(&state, &principal, command)
                .expect("adapter put must succeed")
        else {
            panic!("adapter put must return a StateStoreResult");
        };
        let Some(state_store_result::Result::Entry(entry)) = result.result else {
            panic!("adapter put must return a StateEntry");
        };
        assert_eq!(entry.revision, 1);
        let root =
            crate::config::load_configuration_table(&config_path).expect("config must be readable");
        assert!(
            root.contains_key(ADAPTER_SECTION),
            "adapter must persist its section in config.toml"
        );
        assert!(
            state
                .state_store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(
                    SETTINGS_TEST_NAMESPACE,
                    "alerts/front-door",
                    "local-administrator",
                    true,
                    NOW_MS,
                )
                .is_err(),
            "adapter documents must not duplicate into the registry"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
