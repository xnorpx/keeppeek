//! Persist owner-scoped, revision-aware saved event searches in `config.toml`.

use super::{
    ApiPrincipal, ControlCommandError, ServerState, camera_access,
    event_search_proto::{
        SavedExpressionDoc, collect_references, proto_to_saved_doc, saved_doc_to_proto,
    },
    event_workflow, millis_timestamp, unix_time_ms,
};
use crate::{api::proto, storage::search::expression::EXPRESSION_VERSION};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(super) const CONFIG_SECTION: &str = "saved_event_searches";
const SCHEMA_VERSION: u32 = 1;
const MAX_PER_PRINCIPAL: usize = 64;
const MAX_TOTAL_SEARCHES: usize = 1_024;
const MAX_NAME_BYTES: usize = 128;
const MAX_DRAFT_BYTES: usize = 32 * 1_024;
const MAX_ID_BYTES: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct StoredRegistry {
    schema_version: u32,
    #[serde(default)]
    owners: BTreeMap<String, Vec<StoredSearch>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct StoredSearch {
    id: String,
    name: String,
    revision: u64,
    expression_version: u32,
    expression_json: String,
    updated_at_ms: i64,
}

impl Default for StoredRegistry {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            owners: BTreeMap::new(),
        }
    }
}

/// Validates `[saved_event_searches]` during config load and config-only backup checks.
pub(super) fn validate_configuration(root: &toml::Table) -> anyhow::Result<()> {
    let Some(value) = root.get(CONFIG_SECTION) else {
        return Ok(());
    };
    let registry: StoredRegistry = value.clone().try_into()?;
    validate_registry(&registry)
}

/// Lists saved searches owned by the authenticated principal.
pub(super) fn list(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: &proto::ListSavedEventSearches,
) -> Result<proto::SavedEventSearchResult, ControlCommandError> {
    let owner = event_workflow::actor_id(principal, &request.local_workspace_id)?;
    let access = camera_access::for_principal(state, principal)?;
    let _guard = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = config_path(state)?;
    let registry = load_registry(path)?;
    let records = registry
        .owners
        .get(&owner)
        .map(|items| {
            items
                .iter()
                .map(|item| project_record(state, &access, &owner, item))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(proto::SavedEventSearchResult {
        owner_id: owner,
        searches: records,
        search: None,
        deleted_search_id: String::new(),
    })
}

/// Creates or updates one owner-scoped saved search using compare-and-swap revision checks.
pub(super) fn save(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: &proto::SaveEventSearch,
) -> Result<proto::SavedEventSearchResult, ControlCommandError> {
    let owner = event_workflow::actor_id(principal, &request.local_workspace_id)?;
    let access = camera_access::for_principal(state, principal)?;
    let name = validate_name(&request.name)?;
    if request.expression_version != EXPRESSION_VERSION {
        return Err(invalid(
            "saved search expression version is not supported for saving",
        ));
    }
    let expr = request
        .expression
        .as_ref()
        .ok_or_else(|| invalid("saved search expression is required"))?;
    let doc = proto_to_saved_doc(expr)?;
    let expression_json = serde_json::to_string(&doc)
        .map_err(|_| invalid("saved search expression could not be serialized"))?;
    if expression_json.len() > MAX_DRAFT_BYTES {
        return Err(invalid("saved search draft exceeds 32 KiB"));
    }
    let _guard = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = config_path(state)?;
    let mut registry = load_registry(path)?;
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let saved = if request.search_id.is_empty() {
        create_stored_search(
            &mut registry,
            &owner,
            request,
            name,
            expression_json,
            now_ms,
        )?
    } else {
        update_stored_search(
            &mut registry,
            &owner,
            request,
            name,
            expression_json,
            now_ms,
        )?
    };
    validate_registry(&registry).map_err(|error| invalid(&error.to_string()))?;
    save_registry(path, &registry)?;
    let projected = project_record(state, &access, &owner, &saved);
    Ok(proto::SavedEventSearchResult {
        owner_id: owner,
        searches: Vec::new(),
        search: Some(projected),
        deleted_search_id: String::new(),
    })
}

fn create_stored_search(
    registry: &mut StoredRegistry,
    owner: &str,
    request: &proto::SaveEventSearch,
    name: String,
    expression_json: String,
    now_ms: i64,
) -> Result<StoredSearch, ControlCommandError> {
    if request.expected_revision != 0 {
        return Err(conflict("new saved search must use expected revision zero"));
    }
    enforce_quotas(registry, owner)?;
    let record = StoredSearch {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        revision: 1,
        expression_version: request.expression_version,
        expression_json,
        updated_at_ms: now_ms,
    };
    registry
        .owners
        .entry(owner.to_owned())
        .or_default()
        .push(record.clone());
    Ok(record)
}

fn update_stored_search(
    registry: &mut StoredRegistry,
    owner: &str,
    request: &proto::SaveEventSearch,
    name: String,
    expression_json: String,
    now_ms: i64,
) -> Result<StoredSearch, ControlCommandError> {
    validate_id(&request.search_id)?;
    let entries = registry
        .owners
        .get_mut(owner)
        .ok_or_else(|| not_found("saved search was not found"))?;
    let existing = entries
        .iter_mut()
        .find(|item| item.id == request.search_id)
        .ok_or_else(|| not_found("saved search was not found"))?;
    if existing.expression_version != EXPRESSION_VERSION {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "saved search uses an unsupported schema version and cannot be overwritten",
        ));
    }
    if existing.revision != request.expected_revision {
        return Err(conflict(
            "saved search revision changed; reload before retrying",
        ));
    }
    existing.revision = existing
        .revision
        .checked_add(1)
        .ok_or_else(|| conflict("saved search revision exhausted"))?;
    existing.name = name;
    existing.expression_version = request.expression_version;
    existing.expression_json = expression_json;
    existing.updated_at_ms = now_ms;
    Ok(existing.clone())
}

/// Deletes one owner-scoped saved search after verifying its expected revision.
pub(super) fn delete(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: &proto::DeleteSavedEventSearch,
) -> Result<proto::SavedEventSearchResult, ControlCommandError> {
    let owner = event_workflow::actor_id(principal, &request.local_workspace_id)?;
    validate_id(&request.search_id)?;
    let _guard = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = config_path(state)?;
    let mut registry = load_registry(path)?;
    let entries = registry
        .owners
        .get_mut(&owner)
        .ok_or_else(|| not_found("saved search was not found"))?;
    let position = entries
        .iter()
        .position(|item| item.id == request.search_id)
        .ok_or_else(|| not_found("saved search was not found"))?;
    if entries[position].revision != request.expected_revision {
        return Err(conflict(
            "saved search revision changed; reload before deleting",
        ));
    }
    entries.remove(position);
    if entries.is_empty() {
        registry.owners.remove(&owner);
    }
    save_registry(path, &registry)?;
    Ok(proto::SavedEventSearchResult {
        owner_id: owner,
        searches: Vec::new(),
        search: None,
        deleted_search_id: request.search_id.clone(),
    })
}

fn project_record(
    state: &ServerState,
    access: &crate::access::CameraAccess,
    owner: &str,
    stored: &StoredSearch,
) -> proto::SavedEventSearchRecord {
    let editable = stored.expression_version == EXPRESSION_VERSION;
    let mut expression = None;
    let mut unsupported_draft_json = String::new();
    let mut tombstoned_references = Vec::new();
    if editable {
        match serde_json::from_str::<SavedExpressionDoc>(&stored.expression_json) {
            Ok(doc) => {
                tombstoned_references = detect_tombstones(state, access, &doc);
                expression = saved_doc_to_proto(&doc).ok();
                if expression.is_none() {
                    unsupported_draft_json = stored.expression_json.clone();
                }
            }
            Err(_) => {
                unsupported_draft_json = stored.expression_json.clone();
            }
        }
    } else {
        unsupported_draft_json = stored.expression_json.clone();
    }
    let is_editable = editable && expression.is_some();
    proto::SavedEventSearchRecord {
        search_id: stored.id.clone(),
        owner_id: owner.to_owned(),
        revision: stored.revision,
        name: stored.name.clone(),
        expression_version: stored.expression_version,
        expression,
        unsupported_draft_json,
        updated_at: Some(millis_timestamp(stored.updated_at_ms)),
        tombstoned_references,
        editable: is_editable,
    }
}

fn detect_tombstones(
    state: &ServerState,
    access: &crate::access::CameraAccess,
    doc: &SavedExpressionDoc,
) -> Vec<String> {
    let mut camera_ids = Vec::new();
    let mut group_ids = Vec::new();
    collect_references(doc, &mut camera_ids, &mut group_ids);
    let visible_entries = camera_access::query_cameras(state, access, &[]).unwrap_or_default();
    let visible_cameras: BTreeSet<String> = visible_entries
        .iter()
        .map(|entry| entry.info.id.clone())
        .collect();
    let visible_groups: BTreeSet<String> = visible_entries
        .iter()
        .flat_map(|entry| entry.groups.iter().cloned())
        .collect();
    let mut tombstones = BTreeSet::new();
    for camera in camera_ids {
        if !visible_cameras.contains(&camera) {
            tombstones.insert(format!("camera:{camera}"));
        }
    }
    for group in group_ids {
        if !visible_groups.contains(&group) {
            tombstones.insert(format!("group:{group}"));
        }
    }
    tombstones.into_iter().collect()
}

fn config_path(state: &ServerState) -> Result<&Path, ControlCommandError> {
    state.camera_config_path.as_deref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "saved searches require a writable configuration file",
        )
    })
}

fn load_registry(path: &Path) -> Result<StoredRegistry, ControlCommandError> {
    let root = if path.exists() {
        crate::config::load_configuration_table(path).map_err(|_| storage_error())?
    } else {
        toml::Table::new()
    };
    let Some(value) = root.get(CONFIG_SECTION) else {
        return Ok(StoredRegistry::default());
    };
    let registry: StoredRegistry = value.clone().try_into().map_err(|_| storage_error())?;
    validate_registry(&registry).map_err(|_| storage_error())?;
    Ok(registry)
}

fn save_registry(path: &Path, registry: &StoredRegistry) -> Result<(), ControlCommandError> {
    let mut root = if path.exists() {
        crate::config::load_configuration_table(path).map_err(|_| storage_error())?
    } else {
        toml::Table::new()
    };
    if registry.owners.is_empty() {
        root.remove(CONFIG_SECTION);
    } else {
        let encoded = toml::Value::try_from(registry).map_err(|_| storage_error())?;
        root.insert(CONFIG_SECTION.to_owned(), encoded);
    }
    crate::config::write_configuration_table(path, &root).map_err(|_| storage_error())
}

fn enforce_quotas(registry: &StoredRegistry, owner: &str) -> Result<(), ControlCommandError> {
    let owner_count = registry.owners.get(owner).map_or(0, Vec::len);
    if owner_count >= MAX_PER_PRINCIPAL {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            429,
            "saved search limit per principal reached",
        ));
    }
    let total: usize = registry.owners.values().map(Vec::len).sum();
    if total >= MAX_TOTAL_SEARCHES {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            429,
            "server saved search limit reached",
        ));
    }
    Ok(())
}

fn validate_registry(registry: &StoredRegistry) -> anyhow::Result<()> {
    anyhow::ensure!(
        registry.schema_version == SCHEMA_VERSION,
        "saved_event_searches schema_version is unsupported"
    );
    let mut total = 0usize;
    for (owner, items) in &registry.owners {
        anyhow::ensure!(
            !owner.is_empty() && owner.len() <= 256 && !owner.chars().any(char::is_control),
            "saved search owner identity is invalid"
        );
        anyhow::ensure!(
            items.len() <= MAX_PER_PRINCIPAL,
            "saved search count per owner exceeds {MAX_PER_PRINCIPAL}"
        );
        total = total.saturating_add(items.len());
        anyhow::ensure!(
            total <= MAX_TOTAL_SEARCHES,
            "total saved searches exceed {MAX_TOTAL_SEARCHES}"
        );
        let mut seen = BTreeSet::new();
        for item in items {
            anyhow::ensure!(
                !item.id.is_empty()
                    && item.id.len() <= MAX_ID_BYTES
                    && !item.id.chars().any(char::is_control)
                    && seen.insert(&item.id),
                "saved search ID is invalid or duplicated"
            );
            anyhow::ensure!(
                validate_name(&item.name).is_ok(),
                "saved search name is invalid"
            );
            anyhow::ensure!(item.revision > 0, "saved search revision must be positive");
            anyhow::ensure!(
                item.expression_version > 0,
                "saved search expression version must be positive"
            );
            anyhow::ensure!(
                !item.expression_json.is_empty() && item.expression_json.len() <= MAX_DRAFT_BYTES,
                "saved search expression draft size is invalid"
            );
        }
    }
    Ok(())
}

fn validate_name(raw: &str) -> Result<String, ControlCommandError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_NAME_BYTES || trimmed.chars().any(char::is_control)
    {
        return Err(invalid(
            "saved search name must contain 1 to 128 characters",
        ));
    }
    Ok(trimmed.to_owned())
}

fn validate_id(id: &str) -> Result<(), ControlCommandError> {
    if id.is_empty() || id.len() > MAX_ID_BYTES || id.chars().any(char::is_control) {
        return Err(invalid("saved search ID is invalid"));
    }
    Ok(())
}

fn invalid(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

fn conflict(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Rejected, 409, message)
}

fn not_found(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::NotFound, 404, message)
}

fn storage_error() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        503,
        "saved event searches are currently unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{access::AccessRole, storage::RecordingCatalog};

    fn test_principal(state: &ServerState, name: &str, role: AccessRole) -> ApiPrincipal {
        let issued = state
            .access_manager
            .create_credential(name, None, role, None, 1_000)
            .unwrap();
        let addr = "203.0.113.7".parse().unwrap();
        let auth = format!("Bearer {}", issued.access_key.canonical());
        let authenticated = state
            .access_manager
            .authenticate(addr, &[&auth], 2_000, std::time::Instant::now())
            .unwrap();
        ApiPrincipal::credential(authenticated)
    }

    fn simple_expr(camera_id: &str, group_id: Option<&str>) -> proto::EventSearchExpression {
        let mut children = vec![proto::EventSearchExpression {
            node: Some(proto::event_search_expression::Node::Predicate(
                proto::EventSearchPredicate {
                    kind: Some(proto::event_search_predicate::Kind::CameraIds(
                        proto::EventStringListPredicate {
                            values: vec![camera_id.to_owned()],
                        },
                    )),
                },
            )),
        }];
        if let Some(group_id) = group_id {
            children.push(proto::EventSearchExpression {
                node: Some(proto::event_search_expression::Node::Predicate(
                    proto::EventSearchPredicate {
                        kind: Some(proto::event_search_predicate::Kind::GroupIds(
                            proto::EventStringListPredicate {
                                values: vec![group_id.to_owned()],
                            },
                        )),
                    },
                )),
            });
        }
        proto::EventSearchExpression {
            node: Some(proto::event_search_expression::Node::AndGroup(
                proto::EventSearchExpressionList { children },
            )),
        }
    }

    fn save_request(
        search_id: &str,
        expected_revision: u64,
        name: &str,
        camera_id: &str,
        group_id: Option<&str>,
    ) -> proto::SaveEventSearch {
        proto::SaveEventSearch {
            local_workspace_id: String::new(),
            search_id: search_id.to_owned(),
            expected_revision,
            name: name.to_owned(),
            expression_version: EXPRESSION_VERSION,
            expression: Some(simple_expr(camera_id, group_id)),
            raw_draft_json: String::new(),
        }
    }

    fn verify_future_version_and_backup(
        state: &ServerState,
        alice: &ApiPrincipal,
        config_path: &Path,
    ) {
        let mut root = crate::config::load_configuration_table(config_path).unwrap();
        let mut reg: StoredRegistry = root[CONFIG_SECTION].clone().try_into().unwrap();
        reg.owners
            .entry(alice.id())
            .or_default()
            .push(StoredSearch {
                id: "future-v99".to_owned(),
                name: "Future draft".to_owned(),
                revision: 3,
                expression_version: 99,
                expression_json: r#"{"future":true}"#.to_owned(),
                updated_at_ms: 1_000,
            });
        root.insert(
            CONFIG_SECTION.to_owned(),
            toml::Value::try_from(&reg).unwrap(),
        );
        crate::config::write_configuration_table(config_path, &root).unwrap();
        assert!(validate_configuration(&root).is_ok());

        let alice_list = list(
            state,
            alice,
            &proto::ListSavedEventSearches {
                local_workspace_id: String::new(),
            },
        )
        .unwrap();
        let future = alice_list
            .searches
            .iter()
            .find(|item| item.search_id == "future-v99")
            .unwrap();
        assert!(!future.editable);
        assert_eq!(future.unsupported_draft_json, r#"{"future":true}"#);
        let overwrite_future = save(
            state,
            alice,
            &save_request("future-v99", 3, "Overwrite", "deleted-cam", None),
        )
        .unwrap_err();
        assert_eq!(overwrite_future.code, proto::ErrorCode::Rejected);

        let secrets_path = crate::config::secrets_path(config_path);
        std::fs::write(&secrets_path, "").unwrap();
        let (cursor, _manifest) = crate::backup::create_bundle(
            std::io::Cursor::new(Vec::new()),
            crate::backup::CreateBundleOptions {
                config_path,
                sections: &[],
                created_at_unix_ms: 1_700_000_000_000,
            },
        )
        .unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(cursor.into_inner())).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned())
            .collect();
        assert!(names.iter().any(|name| name == "config.toml"));
        assert!(!names.iter().any(|name| name.ends_with("recordings.db")));
        let mut bundled_config = String::new();
        std::io::Read::read_to_string(
            &mut archive.by_name("config.toml").unwrap(),
            &mut bundled_config,
        )
        .unwrap();
        assert!(bundled_config.contains("[saved_event_searches"));
        assert!(bundled_config.contains("future-v99"));
    }

    #[test]
    fn saved_searches_enforce_owner_isolation_cas_tombstones_and_unsupported_versions() {
        let dir =
            std::env::temp_dir().join(format!("keeppeek-saved-searches-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_path = 'media'\n").unwrap();
        let catalog = RecordingCatalog::open(&dir.join("recordings.db")).unwrap();
        let mut state = ServerState::empty();
        state.catalog = Some(catalog.handle());
        state.camera_config_path = Some(config_path.clone());
        let alice = test_principal(&state, "Alice", AccessRole::User);
        let bob = test_principal(&state, "Bob", AccessRole::User);

        let created = save(
            &state,
            &alice,
            &save_request("", 0, "Missing refs", "deleted-cam", Some("deleted-group")),
        )
        .unwrap();
        let record = created.search.unwrap();
        assert_eq!(record.revision, 1);
        assert!(record.editable);
        assert!(
            record
                .tombstoned_references
                .contains(&"camera:deleted-cam".to_owned())
        );
        assert!(
            record
                .tombstoned_references
                .contains(&"group:deleted-group".to_owned())
        );

        let bob_list = list(
            &state,
            &bob,
            &proto::ListSavedEventSearches {
                local_workspace_id: String::new(),
            },
        )
        .unwrap();
        assert!(bob_list.searches.is_empty());
        assert!(
            save(
                &state,
                &bob,
                &save_request(&record.search_id, 1, "Hijack", "deleted-cam", None),
            )
            .is_err()
        );

        let updated = save(
            &state,
            &alice,
            &save_request(&record.search_id, 1, "Renamed", "deleted-cam", None),
        )
        .unwrap();
        assert_eq!(updated.search.unwrap().revision, 2);
        let conflict = save(
            &state,
            &alice,
            &save_request(&record.search_id, 1, "Stale", "deleted-cam", None),
        )
        .unwrap_err();
        assert_eq!(conflict.code, proto::ErrorCode::Rejected);

        verify_future_version_and_backup(&state, &alice, &config_path);

        delete(
            &state,
            &alice,
            &proto::DeleteSavedEventSearch {
                local_workspace_id: String::new(),
                search_id: record.search_id,
                expected_revision: 2,
            },
        )
        .unwrap();
        drop(state);
        catalog.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_camera_entry(ip: &str, group: &str) -> super::super::CameraEntry {
        let config = crate::cameras::CameraConfig {
            events: Default::default(),
            ip: ip.parse().unwrap(),
            name: None,
            display_name: None,
            manufacturer: None,
            username: String::new(),
            password: String::new(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: crate::cameras::CameraBackend::Retina,
            transport: crate::cameras::CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: crate::cameras::CameraRecordingMode::Main,
            event_recording_duration_secs: 60,
            event_pre_recording_duration_secs: 0,
            event_recording_stream: Default::default(),
            recorded_audio: Default::default(),
            live_audio: Default::default(),
        };
        let mut entry = super::super::camera_entry(&config, None);
        entry.groups = vec![group.to_owned()];
        entry
    }

    #[test]
    fn tombstone_check_reports_deleted_and_unauthorized_cameras_and_groups() {
        let mut state = ServerState::empty();
        state.cameras = std::sync::Arc::new(std::sync::RwLock::new(vec![
            test_camera_entry("192.0.2.10", "Exterior"),
            test_camera_entry("192.0.2.11", "Interior"),
        ]));
        let access = crate::access::CameraAccess {
            all_cameras: false,
            camera_ids: vec!["192.0.2.10".to_owned()],
            group_ids: Vec::new(),
        };
        let doc = proto_to_saved_doc(&proto::EventSearchExpression {
            node: Some(proto::event_search_expression::Node::AndGroup(
                proto::EventSearchExpressionList {
                    children: vec![
                        proto::EventSearchExpression {
                            node: Some(proto::event_search_expression::Node::Predicate(
                                proto::EventSearchPredicate {
                                    kind: Some(proto::event_search_predicate::Kind::CameraIds(
                                        proto::EventStringListPredicate {
                                            values: vec![
                                                "192.0.2.10".to_owned(),
                                                "192.0.2.11".to_owned(),
                                                "deleted-cam".to_owned(),
                                            ],
                                        },
                                    )),
                                },
                            )),
                        },
                        proto::EventSearchExpression {
                            node: Some(proto::event_search_expression::Node::Predicate(
                                proto::EventSearchPredicate {
                                    kind: Some(proto::event_search_predicate::Kind::GroupIds(
                                        proto::EventStringListPredicate {
                                            values: vec![
                                                "Exterior".to_owned(),
                                                "Interior".to_owned(),
                                                "deleted-group".to_owned(),
                                            ],
                                        },
                                    )),
                                },
                            )),
                        },
                    ],
                },
            )),
        })
        .unwrap();
        let tombstones = detect_tombstones(&state, &access, &doc);
        assert_eq!(
            tombstones,
            vec![
                "camera:192.0.2.11".to_owned(),
                "camera:deleted-cam".to_owned(),
                "group:Interior".to_owned(),
                "group:deleted-group".to_owned(),
            ]
        );
    }
}
