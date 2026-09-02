use super::{ApiPrincipal, ControlCommandError, ServerState};
use crate::{
    access::AccessRole,
    api::proto::{self, ok as control_ok, state_store_command, state_store_result},
};
use prost::Message as _;
use prost_types::{ListValue, Struct, Timestamp, Value, value::Kind};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    path::PathBuf,
    time::SystemTime,
};

pub(super) const CAPABILITY_ID: &str = "keeppeek.peek-layouts.v1";
pub(super) const NAMESPACE: &str = "keeppeek.peek-layouts";
const REGISTRY_KEY: &str = "registry";
const REGISTRY_SCHEMA: &str = "keeppeek.peek-layout-registry.v1";
const SCHEMA_VERSION: u32 = 1;
const GRID_COLUMNS: u32 = 12;
const GRID_ROWS: u32 = 12;
const MAX_LAYOUTS: usize = 32;
const MAX_TILES: usize = 64;
const MAX_VIEWERS: usize = 128;
const MAX_LAYOUT_ID_CHARS: usize = 128;
const MAX_LAYOUT_NAME_CHARS: usize = 80;
const MAX_REGISTRY_BYTES: usize = 256 * 1_024;
const DEFAULT_LAYOUT_ID: &str = "default";
const SHARED_OWNER_ID: &str = "server";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LayoutRegistry {
    #[serde(deserialize_with = "deserialize_whole_u32")]
    schema_version: u32,
    active_layout_id: String,
    layouts: Vec<Layout>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Layout {
    id: String,
    name: String,
    scope: LayoutScope,
    owner_id: String,
    #[serde(default)]
    audience: LayoutAudience,
    activity_focus: bool,
    tiles: Vec<LayoutTile>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LayoutAudience {
    everyone: bool,
    credential_ids: Vec<String>,
}

impl Default for LayoutAudience {
    fn default() -> Self {
        Self {
            everyone: true,
            credential_ids: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum LayoutScope {
    Private,
    Shared,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LayoutTile {
    camera_id: String,
    #[serde(deserialize_with = "deserialize_whole_u32")]
    column: u32,
    #[serde(deserialize_with = "deserialize_whole_u32")]
    row: u32,
    #[serde(deserialize_with = "deserialize_whole_u32")]
    column_span: u32,
    #[serde(deserialize_with = "deserialize_whole_u32")]
    row_span: u32,
    pinned: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredRegistry {
    schema_version: u32,
    revision: u64,
    shared_layouts: Vec<Layout>,
    users: BTreeMap<String, StoredUserRegistry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredUserRegistry {
    active_layout_id: String,
    layouts: Vec<Layout>,
}

struct RegistryStore {
    path: PathBuf,
    camera_ids: HashSet<String>,
    registry: StoredRegistry,
}

pub(super) fn validate_backup_document(bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.len() > MAX_REGISTRY_BYTES {
        anyhow::bail!("layout registry exceeds its size limit");
    }
    let registry: StoredRegistry = serde_json::from_slice(bytes)?;
    RegistryStore {
        path: PathBuf::new(),
        camera_ids: HashSet::new(),
        registry,
    }
    .validate_stored()
    .map_err(|error| anyhow::anyhow!(error.to_string()))
}

pub(super) fn backup_camera_ids(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    if bytes.len() > MAX_REGISTRY_BYTES {
        anyhow::bail!("layout registry exceeds its size limit");
    }
    let registry: StoredRegistry = serde_json::from_slice(bytes)?;
    let mut camera_ids = registry
        .shared_layouts
        .iter()
        .chain(registry.users.values().flat_map(|user| user.layouts.iter()))
        .flat_map(|layout| layout.tiles.iter().map(|tile| tile.camera_id.clone()))
        .collect::<Vec<_>>();
    camera_ids.sort_unstable();
    camera_ids.dedup();
    Ok(camera_ids)
}

#[derive(Debug, PartialEq, Eq)]
enum RegistryError {
    Conflict { current_revision: u64 },
    Invalid(String),
    NotAuthorized { current_revision: u64 },
    Storage(String),
}

impl RegistryError {
    fn new(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    fn storage(message: impl Into<String>) -> Self {
        Self::Storage(message.into())
    }
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict { current_revision } => write!(
                formatter,
                "layout registry revision conflict (current {current_revision})"
            ),
            Self::NotAuthorized { .. } => {
                formatter.write_str("shared layouts require administrator access")
            }
            Self::Invalid(message) | Self::Storage(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for RegistryError {}

fn deserialize_whole_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = f64::deserialize(deserializer)?;
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > f64::from(u32::MAX) {
        return Err(serde::de::Error::custom(
            "value must be a nonnegative whole number",
        ));
    }
    Ok(value as u32)
}

pub(super) fn dispatch(
    state: &ServerState,
    principal: &ApiPrincipal,
    command: proto::StateStoreCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    let result = match command.action {
        Some(state_store_command::Action::Get(request)) => get(state, principal, request)?,
        Some(state_store_command::Action::Put(request)) => put(state, principal, request)?,
        Some(
            state_store_command::Action::Delete(_)
            | state_store_command::Action::Watch(_)
            | state_store_command::Action::Unwatch(_),
        ) => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::UnsupportedRequest,
                501,
                "this Peek layout StateStore operation is not implemented",
            ));
        }
        None => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "state store command has no action",
            ));
        }
    };
    Ok(control_ok::Result::StateStoreResult(result))
}

pub(super) fn handles(command: &proto::StateStoreCommand) -> bool {
    match &command.action {
        Some(state_store_command::Action::Get(request)) => request.namespace == NAMESPACE,
        Some(state_store_command::Action::Put(request)) => request.namespace == NAMESPACE,
        Some(state_store_command::Action::Delete(request)) => request.namespace == NAMESPACE,
        Some(state_store_command::Action::Watch(request)) => request.namespace == NAMESPACE,
        Some(state_store_command::Action::Unwatch(_)) | None => false,
    }
}

fn get(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::GetState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    require_target(&request.namespace, &request.key)?;
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let store = open_store(state)?;
    state_entry(&store, principal)
}

fn put(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::PutState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    require_target(&request.namespace, &request.key)?;
    if request.schema != REGISTRY_SCHEMA {
        return Err(invalid("Peek layout registry schema is invalid"));
    }
    if request.ttl.is_some() {
        return Err(invalid("Peek layout registries do not support a TTL"));
    }
    let expected_revision = request
        .expected_revision
        .ok_or_else(|| invalid("Peek layout registry revision is required"))?;
    let value = request
        .value
        .ok_or_else(|| invalid("Peek layout registry value is required"))?;
    let json = struct_to_json(value);
    let encoded =
        serde_json::to_vec(&json).map_err(|_| invalid("Peek layout registry value is invalid"))?;
    if encoded.len() > MAX_REGISTRY_BYTES {
        return Err(invalid("Peek layout registry value is too large"));
    }
    let candidate: LayoutRegistry = serde_json::from_value(json)
        .map_err(|_| invalid("Peek layout registry value is invalid"))?;
    let known_credential_ids = state
        .access_manager
        .list_credentials()
        .into_iter()
        .map(|credential| credential.id.to_string())
        .collect();
    candidate
        .validate_viewer_identities(&known_credential_ids)
        .map_err(|error| registry_command_error(error, &request.namespace, &request.key))?;
    let principal_id = principal.id();
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut store = open_store(state)?;
    store
        .replace_for(
            &principal_id,
            principal.role == AccessRole::Administrator,
            expected_revision,
            candidate,
        )
        .map_err(|error| registry_command_error(error, &request.namespace, &request.key))?;
    state_entry(&store, principal)
}

fn open_store(state: &ServerState) -> Result<RegistryStore, ControlCommandError> {
    let config_path = state.camera_config_path.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "Peek layout persistence is unavailable",
        )
    })?;
    let path = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("peek-layouts.json");
    let camera_ids = state
        .camera_entries()
        .into_iter()
        .map(|camera| camera.info.id)
        .collect::<Vec<_>>();
    RegistryStore::open(path, &camera_ids).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("Peek layout registry could not be opened: {error}"),
        )
    })
}

fn state_entry(
    store: &RegistryStore,
    principal: &ApiPrincipal,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let principal_id = principal.id();
    let value = serde_json::to_value(
        store.registry_for_principal(&principal_id, principal.role == AccessRole::Administrator),
    )
    .map_err(|_| internal("Peek layout registry could not be encoded"))?;
    let value =
        json_to_struct(value).map_err(|_| internal("Peek layout registry could not be encoded"))?;
    Ok(proto::StateStoreResult {
        result: Some(state_store_result::Result::Entry(proto::StateEntry {
            namespace: NAMESPACE.to_owned(),
            key: REGISTRY_KEY.to_owned(),
            schema: REGISTRY_SCHEMA.to_owned(),
            value: Some(value),
            revision: store.revision(),
            updated_at: Some(now_timestamp()),
            expires_at: None,
            owner_id: principal_id,
        })),
    })
}

fn require_target(namespace: &str, key: &str) -> Result<(), ControlCommandError> {
    if namespace != NAMESPACE {
        return Err(invalid("Peek layout namespace is invalid"));
    }
    if key != REGISTRY_KEY {
        return Err(invalid("Peek layout registry key is invalid"));
    }
    Ok(())
}

fn registry_command_error(error: RegistryError, namespace: &str, key: &str) -> ControlCommandError {
    match error {
        RegistryError::Conflict { current_revision } => ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "Peek layout registry revision conflict",
        )
        .with_detail(prost_types::Any {
            type_url: "type.googleapis.com/keeppeek.webrtc.v1.StateStoreError".to_owned(),
            value: proto::StateStoreError {
                namespace: namespace.to_owned(),
                key: key.to_owned(),
                code: proto::StateStoreErrorCode::Conflict as i32,
                current_revision: Some(current_revision),
            }
            .encode_to_vec(),
        }),
        RegistryError::NotAuthorized { current_revision } => ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "shared layouts require administrator access",
        )
        .with_detail(prost_types::Any {
            type_url: "type.googleapis.com/keeppeek.webrtc.v1.StateStoreError".to_owned(),
            value: proto::StateStoreError {
                namespace: namespace.to_owned(),
                key: key.to_owned(),
                code: proto::StateStoreErrorCode::NotAuthorized as i32,
                current_revision: Some(current_revision),
            }
            .encode_to_vec(),
        }),
        RegistryError::Invalid(message) => invalid(&message),
        RegistryError::Storage(message) => internal(&message),
    }
}

fn json_to_struct(value: serde_json::Value) -> anyhow::Result<Struct> {
    let serde_json::Value::Object(fields) = value else {
        anyhow::bail!("state value is not an object");
    };
    Ok(Struct {
        fields: fields
            .into_iter()
            .map(|(key, value)| (key, json_to_value(value)))
            .collect(),
    })
}

fn json_to_value(value: serde_json::Value) -> Value {
    let kind = match value {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(value) => Kind::BoolValue(value),
        serde_json::Value::Number(value) => Kind::NumberValue(value.as_f64().unwrap_or_default()),
        serde_json::Value::String(value) => Kind::StringValue(value),
        serde_json::Value::Array(values) => Kind::ListValue(ListValue {
            values: values.into_iter().map(json_to_value).collect(),
        }),
        serde_json::Value::Object(fields) => Kind::StructValue(Struct {
            fields: fields
                .into_iter()
                .map(|(key, value)| (key, json_to_value(value)))
                .collect(),
        }),
    };
    Value { kind: Some(kind) }
}

fn struct_to_json(value: Struct) -> serde_json::Value {
    serde_json::Value::Object(
        value
            .fields
            .into_iter()
            .map(|(key, value)| (key, value_to_json(value)))
            .collect(),
    )
}

fn value_to_json(value: Value) -> serde_json::Value {
    match value.kind {
        Some(Kind::NullValue(_)) | None => serde_json::Value::Null,
        Some(Kind::NumberValue(value)) => serde_json::Number::from_f64(value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Some(Kind::StringValue(value)) => serde_json::Value::String(value),
        Some(Kind::BoolValue(value)) => serde_json::Value::Bool(value),
        Some(Kind::StructValue(value)) => struct_to_json(value),
        Some(Kind::ListValue(value)) => {
            serde_json::Value::Array(value.values.into_iter().map(value_to_json).collect())
        }
    }
}

fn now_timestamp() -> Timestamp {
    let elapsed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp {
        seconds: i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(elapsed.subsec_nanos()).unwrap_or_default(),
    }
}

fn invalid(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

fn internal(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Internal, 500, message)
}

impl RegistryStore {
    fn open(path: PathBuf, camera_ids: &[String]) -> Result<Self, RegistryError> {
        let mut canonical_camera_ids = camera_ids.to_vec();
        canonical_camera_ids.sort_unstable();
        canonical_camera_ids.dedup();
        let stored = path.exists();
        let registry = if stored {
            let size = std::fs::metadata(&path)
                .map_err(|error| RegistryError::storage(error.to_string()))?
                .len();
            if size > MAX_REGISTRY_BYTES as u64 {
                return Err(RegistryError::storage(
                    "stored layout registry is too large",
                ));
            }
            let bytes =
                std::fs::read(&path).map_err(|error| RegistryError::storage(error.to_string()))?;
            serde_json::from_slice(&bytes)
                .map_err(|_| RegistryError::storage("stored layout registry is invalid"))?
        } else {
            StoredRegistry::new(&canonical_camera_ids)
        };
        let mut store = Self {
            path,
            camera_ids: canonical_camera_ids.iter().cloned().collect(),
            registry,
        };
        store.validate_stored()?;
        let migrated = store.migrate_private_layouts();
        let synchronized = store.synchronize_default(&canonical_camera_ids);
        store.validate_stored()?;
        if !stored || migrated || synchronized {
            store.persist(&store.registry)?;
        }
        Ok(store)
    }

    fn migrate_private_layouts(&mut self) -> bool {
        let mut used_ids = self
            .registry
            .shared_layouts
            .iter()
            .map(|layout| layout.id.clone())
            .collect::<HashSet<_>>();
        let mut migrated = Vec::new();
        for (principal_id, user) in &mut self.registry.users {
            for mut layout in std::mem::take(&mut user.layouts) {
                let previous_id = layout.id.clone();
                while !used_ids.insert(layout.id.clone()) {
                    layout.id = uuid::Uuid::new_v4().to_string();
                }
                if user.active_layout_id == previous_id {
                    user.active_layout_id.clone_from(&layout.id);
                }
                layout.scope = LayoutScope::Shared;
                layout.owner_id = SHARED_OWNER_ID.to_owned();
                layout.audience = LayoutAudience {
                    everyone: false,
                    credential_ids: uuid::Uuid::parse_str(principal_id)
                        .ok()
                        .filter(|identity| identity.to_string() == *principal_id)
                        .map(|_| vec![principal_id.clone()])
                        .unwrap_or_default(),
                };
                migrated.push(layout);
            }
        }
        if migrated.is_empty() {
            return false;
        }
        self.registry.shared_layouts.extend(migrated);
        self.registry.revision = self.registry.revision.saturating_add(1);
        self.registry.repair_active_layouts();
        true
    }

    fn synchronize_default(&mut self, camera_ids: &[String]) -> bool {
        let default = default_layout(camera_ids);
        let default_index = self
            .registry
            .shared_layouts
            .iter()
            .position(|layout| layout.id == DEFAULT_LAYOUT_ID);
        if default_index == Some(0) && self.registry.shared_layouts[0] == default {
            return false;
        }
        if let Some(index) = default_index {
            self.registry.shared_layouts.remove(index);
        }
        self.registry.shared_layouts.insert(0, default);
        self.registry.revision = self.registry.revision.saturating_add(1);
        self.registry.repair_active_layouts();
        true
    }

    const fn revision(&self) -> u64 {
        self.registry.revision
    }

    fn registry_for(&self, principal_id: &str) -> LayoutRegistry {
        self.registry_for_principal(principal_id, false)
    }

    fn registry_for_principal(&self, principal_id: &str, administrator: bool) -> LayoutRegistry {
        let user = self.registry.users.get(principal_id);
        let mut layouts = self
            .registry
            .shared_layouts
            .iter()
            .filter(|layout| administrator || layout.is_visible_to(principal_id))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(user) = user {
            layouts.extend(user.layouts.clone());
        }
        let active_layout_id = user
            .map(|user| user.active_layout_id.as_str())
            .filter(|active_id| layouts.iter().any(|layout| layout.id == *active_id))
            .or_else(|| {
                layouts
                    .iter()
                    .find(|layout| layout.id == DEFAULT_LAYOUT_ID)
                    .map(|layout| layout.id.as_str())
            })
            .or_else(|| layouts.first().map(|layout| layout.id.as_str()))
            .unwrap_or(DEFAULT_LAYOUT_ID)
            .to_owned();
        LayoutRegistry {
            schema_version: SCHEMA_VERSION,
            active_layout_id,
            layouts,
        }
    }

    fn replace_for(
        &mut self,
        principal_id: &str,
        administrator: bool,
        expected_revision: u64,
        candidate: LayoutRegistry,
    ) -> Result<(), RegistryError> {
        if expected_revision != self.registry.revision {
            return Err(RegistryError::Conflict {
                current_revision: self.registry.revision,
            });
        }

        let mut allowed_camera_ids = self.camera_ids.clone();
        let private_layouts = self
            .registry
            .users
            .get(principal_id)
            .into_iter()
            .flat_map(|user| user.layouts.iter());
        for layout in self.registry.shared_layouts.iter().chain(private_layouts) {
            allowed_camera_ids.extend(layout.tiles.iter().map(|tile| tile.camera_id.clone()));
        }
        candidate.validate(principal_id, &allowed_camera_ids)?;
        if !administrator {
            let current = self.registry_for(principal_id);
            if candidate.layouts != current.layouts {
                return Err(RegistryError::NotAuthorized {
                    current_revision: self.registry.revision,
                });
            }
            let mut next = self.registry.clone();
            next.users.insert(
                principal_id.to_owned(),
                StoredUserRegistry {
                    active_layout_id: candidate.active_layout_id,
                    layouts: next
                        .users
                        .get(principal_id)
                        .map(|user| user.layouts.clone())
                        .unwrap_or_default(),
                },
            );
            next.revision = next.revision.saturating_add(1);
            self.persist(&next)?;
            self.registry = next;
            return Ok(());
        }
        let (shared_layouts, private_layouts): (Vec<_>, Vec<_>) = candidate
            .layouts
            .into_iter()
            .partition(|layout| layout.scope == LayoutScope::Shared);
        if !private_layouts.is_empty() {
            return Err(RegistryError::new(
                "dashboard registry cannot contain private layouts",
            ));
        }
        if shared_layouts.is_empty() {
            return Err(RegistryError::new(
                "layout registry must retain at least one shared layout",
            ));
        }
        if shared_layouts
            .iter()
            .any(|layout| layout.owner_id != SHARED_OWNER_ID)
        {
            return Err(RegistryError::new("shared layout owner is invalid"));
        }
        let mut camera_ids = self.camera_ids.iter().cloned().collect::<Vec<_>>();
        camera_ids.sort_unstable();
        let default = default_layout(&camera_ids);
        if shared_layouts
            .iter()
            .find(|layout| layout.id == DEFAULT_LAYOUT_ID)
            != Some(&default)
        {
            return Err(RegistryError::new(
                "the All cameras dashboard cannot be changed",
            ));
        }

        let mut next = self.registry.clone();
        next.shared_layouts = shared_layouts;
        next.users.insert(
            principal_id.to_owned(),
            StoredUserRegistry {
                active_layout_id: candidate.active_layout_id,
                layouts: private_layouts,
            },
        );
        next.revision = next.revision.saturating_add(1);
        next.repair_active_layouts();
        self.persist(&next)?;
        self.registry = next;
        Ok(())
    }

    fn validate_stored(&self) -> Result<(), RegistryError> {
        if self.registry.schema_version != SCHEMA_VERSION || self.registry.revision == 0 {
            return Err(RegistryError::new("stored layout registry is invalid"));
        }
        if self.registry.shared_layouts.is_empty()
            || self.registry.shared_layouts.iter().any(|layout| {
                layout.scope != LayoutScope::Shared || layout.owner_id != SHARED_OWNER_ID
            })
        {
            return Err(RegistryError::new("stored shared layouts are invalid"));
        }
        let shared_view = LayoutRegistry {
            schema_version: SCHEMA_VERSION,
            active_layout_id: self.registry.shared_layouts[0].id.clone(),
            layouts: self.registry.shared_layouts.clone(),
        };
        let retained_shared_camera_ids = shared_view
            .layouts
            .iter()
            .flat_map(|layout| layout.tiles.iter().map(|tile| tile.camera_id.clone()))
            .chain(self.camera_ids.iter().cloned())
            .collect();
        shared_view.validate(SHARED_OWNER_ID, &retained_shared_camera_ids)?;
        for (principal_id, user) in &self.registry.users {
            let view = LayoutRegistry {
                schema_version: SCHEMA_VERSION,
                active_layout_id: user.active_layout_id.clone(),
                layouts: self
                    .registry
                    .shared_layouts
                    .iter()
                    .cloned()
                    .chain(user.layouts.iter().cloned())
                    .collect(),
            };
            let retained_camera_ids = view
                .layouts
                .iter()
                .flat_map(|layout| layout.tiles.iter().map(|tile| tile.camera_id.clone()))
                .chain(self.camera_ids.iter().cloned())
                .collect();
            view.validate(principal_id, &retained_camera_ids)?;
        }
        Ok(())
    }

    fn persist(&self, registry: &StoredRegistry) -> Result<(), RegistryError> {
        let bytes = serde_json::to_vec_pretty(registry)
            .map_err(|_| RegistryError::storage("layout registry could not be encoded"))?;
        if bytes.len() > MAX_REGISTRY_BYTES {
            return Err(RegistryError::storage("layout registry is too large"));
        }
        crate::config::write_private_file_atomically(&self.path, &bytes)
            .map_err(|error| RegistryError::storage(error.to_string()))
    }
}

impl StoredRegistry {
    fn new(camera_ids: &[String]) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 1,
            shared_layouts: vec![default_layout(camera_ids)],
            users: BTreeMap::new(),
        }
    }

    fn repair_active_layouts(&mut self) {
        let shared_ids: HashSet<_> = self
            .shared_layouts
            .iter()
            .map(|layout| layout.id.as_str())
            .collect();
        for user in self.users.values_mut() {
            if shared_ids.contains(user.active_layout_id.as_str())
                || user
                    .layouts
                    .iter()
                    .any(|layout| layout.id == user.active_layout_id)
            {
                continue;
            }
            user.active_layout_id = user
                .layouts
                .first()
                .map(|layout| layout.id.clone())
                .unwrap_or_else(|| self.shared_layouts[0].id.clone());
        }
    }
}

fn default_layout(camera_ids: &[String]) -> Layout {
    let slots = default_slots(camera_ids.len().min(MAX_TILES));
    Layout {
        id: DEFAULT_LAYOUT_ID.to_owned(),
        name: "All cameras".to_owned(),
        scope: LayoutScope::Shared,
        owner_id: SHARED_OWNER_ID.to_owned(),
        audience: LayoutAudience::default(),
        activity_focus: true,
        tiles: camera_ids
            .iter()
            .take(MAX_TILES)
            .zip(slots)
            .enumerate()
            .map(
                |(index, (camera_id, (column, row, column_span, row_span)))| LayoutTile {
                    camera_id: camera_id.clone(),
                    column,
                    row,
                    column_span,
                    row_span,
                    pinned: index == 0,
                },
            )
            .collect(),
    }
}

fn default_slots(camera_count: usize) -> Vec<(u32, u32, u32, u32)> {
    match camera_count {
        0 => return Vec::new(),
        1 => return vec![(1, 1, 12, 12)],
        2 => return vec![(1, 1, 6, 12), (7, 1, 6, 12)],
        3 => return vec![(1, 1, 8, 12), (9, 1, 4, 6), (9, 7, 4, 6)],
        4 => return vec![(1, 1, 8, 12), (9, 1, 4, 4), (9, 9, 4, 4), (9, 5, 4, 4)],
        _ => {}
    }
    let columns = match camera_count {
        5..=9 => 3,
        10..=16 => 4,
        17..=36 => 6,
        _ => 12,
    };
    let rows = camera_count.div_ceil(columns);
    let column_span = 12 / u32::try_from(columns).unwrap_or(12);
    let row_span = 12 / u32::try_from(rows).unwrap_or(12);
    (0..camera_count)
        .map(|index| {
            let column = u32::try_from(index % columns).unwrap_or_default() * column_span + 1;
            let row = u32::try_from(index / columns).unwrap_or_default() * row_span + 1;
            (column, row, column_span, row_span)
        })
        .collect()
}

impl LayoutRegistry {
    #[cfg(test)]
    fn from_json(
        value: serde_json::Value,
        principal_id: &str,
        camera_ids: &HashSet<String>,
    ) -> Result<Self, RegistryError> {
        let registry: Self = serde_json::from_value(value)
            .map_err(|_| RegistryError::new("layout registry is invalid"))?;
        registry.validate(principal_id, camera_ids)?;
        Ok(registry)
    }

    fn validate(
        &self,
        principal_id: &str,
        camera_ids: &HashSet<String>,
    ) -> Result<(), RegistryError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(RegistryError::new(
                "layout registry schema version is unsupported",
            ));
        }
        if self.layouts.is_empty() || self.layouts.len() > MAX_LAYOUTS {
            return Err(RegistryError::new("layout registry size is invalid"));
        }

        let mut layout_ids = HashSet::with_capacity(self.layouts.len());
        for layout in &self.layouts {
            if layout.id.trim().is_empty() || layout.id.chars().count() > MAX_LAYOUT_ID_CHARS {
                return Err(RegistryError::new("layout ID is invalid"));
            }
            if !layout_ids.insert(layout.id.as_str()) {
                return Err(RegistryError::new("layout IDs must be unique"));
            }
            if layout.name.trim().is_empty() || layout.name.chars().count() > MAX_LAYOUT_NAME_CHARS
            {
                return Err(RegistryError::new("layout name is invalid"));
            }
            if layout.scope == LayoutScope::Private && layout.owner_id != principal_id {
                return Err(RegistryError::new("private layout owner is invalid"));
            }
            layout.validate(camera_ids)?;
        }
        if !layout_ids.contains(self.active_layout_id.as_str()) {
            return Err(RegistryError::new("active layout does not exist"));
        }
        Ok(())
    }

    fn validate_viewer_identities(
        &self,
        known_credential_ids: &HashSet<String>,
    ) -> Result<(), RegistryError> {
        if self.layouts.iter().any(|layout| {
            layout
                .audience
                .credential_ids
                .iter()
                .any(|identity| !known_credential_ids.contains(identity))
        }) {
            return Err(RegistryError::new(
                "dashboard viewer identity does not exist",
            ));
        }
        Ok(())
    }
}

impl Layout {
    fn validate(&self, camera_ids: &HashSet<String>) -> Result<(), RegistryError> {
        self.audience.validate()?;
        if self.tiles.len() > MAX_TILES {
            return Err(RegistryError::new("layout has too many tiles"));
        }
        let mut placed_camera_ids = HashSet::with_capacity(self.tiles.len());
        for (index, tile) in self.tiles.iter().enumerate() {
            if !camera_ids.contains(&tile.camera_id) {
                return Err(RegistryError::new("layout contains an unknown camera"));
            }
            if !placed_camera_ids.insert(tile.camera_id.as_str()) {
                return Err(RegistryError::new("layout contains a duplicate camera"));
            }
            if !tile.is_in_bounds() {
                return Err(RegistryError::new("layout tile is outside the grid"));
            }
            if self.tiles[..index].iter().any(|other| tile.overlaps(other)) {
                return Err(RegistryError::new("layout tiles must not overlap"));
            }
        }
        Ok(())
    }

    fn is_visible_to(&self, principal_id: &str) -> bool {
        self.audience.everyone
            || self
                .audience
                .credential_ids
                .iter()
                .any(|credential_id| credential_id == principal_id)
    }
}

impl LayoutAudience {
    fn validate(&self) -> Result<(), RegistryError> {
        if self.credential_ids.len() > MAX_VIEWERS
            || (self.everyone && !self.credential_ids.is_empty())
        {
            return Err(RegistryError::new("dashboard audience is invalid"));
        }
        let mut identities = HashSet::with_capacity(self.credential_ids.len());
        for credential_id in &self.credential_ids {
            let parsed = uuid::Uuid::parse_str(credential_id)
                .map_err(|_| RegistryError::new("dashboard viewer identity is invalid"))?;
            if parsed.to_string() != *credential_id || !identities.insert(credential_id.as_str()) {
                return Err(RegistryError::new(
                    "dashboard viewer identities must be canonical and unique",
                ));
            }
        }
        Ok(())
    }
}

impl LayoutTile {
    const fn is_in_bounds(&self) -> bool {
        self.column >= 1
            && self.row >= 1
            && self.column_span >= 1
            && self.row_span >= 1
            && self.column + self.column_span <= GRID_COLUMNS + 1
            && self.row + self.row_span <= GRID_ROWS + 1
    }

    const fn overlaps(&self, other: &Self) -> bool {
        self.column < other.column + other.column_span
            && self.column + self.column_span > other.column
            && self.row < other.row + other.row_span
            && self.row + self.row_span > other.row
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::AuthenticatedCredential;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn registry_validation_rejects_overlapping_tiles() {
        let camera_ids = HashSet::from(["front-door".to_owned(), "driveway".to_owned()]);
        let value = serde_json::json!({
            "schema_version": 1,
            "active_layout_id": "front-of-house",
            "layouts": [{
                "id": "front-of-house",
                "name": "Front of house",
                "scope": "private",
                "owner_id": "operator-1",
                "activity_focus": true,
                "tiles": [
                    {
                        "camera_id": "front-door",
                        "column": 1,
                        "row": 1,
                        "column_span": 8,
                        "row_span": 12,
                        "pinned": true
                    },
                    {
                        "camera_id": "driveway",
                        "column": 8,
                        "row": 1,
                        "column_span": 4,
                        "row_span": 4,
                        "pinned": false
                    }
                ]
            }]
        });

        let error = LayoutRegistry::from_json(value, "operator-1", &camera_ids).unwrap_err();

        assert_eq!(error.to_string(), "layout tiles must not overlap");
    }

    #[test]
    fn registry_validation_rejects_unknown_duplicate_out_of_bounds_and_oversized_values() {
        let camera_ids = HashSet::from(["front-door".to_owned()]);
        let layout = |tiles: serde_json::Value| {
            serde_json::json!({
                "id": "layout",
                "name": "Layout",
                "scope": "private",
                "owner_id": "operator-1",
                "activity_focus": true,
                "tiles": tiles
            })
        };
        let registry = |layouts: serde_json::Value| {
            serde_json::json!({
                "schema_version": 1,
                "active_layout_id": "layout",
                "layouts": layouts
            })
        };
        let tile = |camera_id: &str, column: u32, column_span: u32| {
            serde_json::json!({
                "camera_id": camera_id,
                "column": column,
                "row": 1,
                "column_span": column_span,
                "row_span": 12,
                "pinned": false
            })
        };

        let unknown = LayoutRegistry::from_json(
            registry(serde_json::json!([layout(serde_json::json!([tile(
                "unknown", 1, 12
            )]))])),
            "operator-1",
            &camera_ids,
        )
        .unwrap_err();
        assert_eq!(unknown.to_string(), "layout contains an unknown camera");

        let duplicate = LayoutRegistry::from_json(
            registry(serde_json::json!([layout(serde_json::json!([
                tile("front-door", 1, 6),
                tile("front-door", 7, 6)
            ]))])),
            "operator-1",
            &camera_ids,
        )
        .unwrap_err();
        assert_eq!(duplicate.to_string(), "layout contains a duplicate camera");

        let out_of_bounds = LayoutRegistry::from_json(
            registry(serde_json::json!([layout(serde_json::json!([tile(
                "front-door",
                8,
                6
            )]))])),
            "operator-1",
            &camera_ids,
        )
        .unwrap_err();
        assert_eq!(out_of_bounds.to_string(), "layout tile is outside the grid");

        let layouts = (0..=MAX_LAYOUTS)
            .map(|index| {
                serde_json::json!({
                    "id": if index == 0 { "layout".to_owned() } else { format!("layout-{index}") },
                    "name": format!("Layout {index}"),
                    "scope": "private",
                    "owner_id": "operator-1",
                    "activity_focus": true,
                    "tiles": []
                })
            })
            .collect::<Vec<_>>();
        let oversized =
            LayoutRegistry::from_json(registry(layouts.into()), "operator-1", &camera_ids)
                .unwrap_err();
        assert_eq!(oversized.to_string(), "layout registry size is invalid");

        let long_id = "x".repeat(129);
        let invalid_id = LayoutRegistry::from_json(
            serde_json::json!({
                "schema_version": 1,
                "active_layout_id": long_id,
                "layouts": [{
                    "id": long_id,
                    "name": "Layout",
                    "scope": "private",
                    "owner_id": "operator-1",
                    "activity_focus": true,
                    "tiles": []
                }]
            }),
            "operator-1",
            &camera_ids,
        )
        .unwrap_err();
        assert_eq!(invalid_id.to_string(), "layout ID is invalid");
    }

    #[test]
    fn default_layout_includes_all_configured_cameras_without_overlap() {
        let camera_ids = (1..=6)
            .map(|index| format!("camera-{index}"))
            .collect::<Vec<_>>();

        let layout = default_layout(&camera_ids);

        assert_eq!(layout.name, "All cameras");
        assert_eq!(
            layout
                .tiles
                .iter()
                .map(|tile| tile.camera_id.as_str())
                .collect::<Vec<_>>(),
            camera_ids.iter().map(String::as_str).collect::<Vec<_>>()
        );
        for (index, tile) in layout.tiles.iter().enumerate() {
            assert!(tile.is_in_bounds());
            assert!(
                !layout.tiles[..index]
                    .iter()
                    .any(|other| tile.overlaps(other))
            );
        }

        let one = default_layout(&["front-door".to_owned()]);
        assert_eq!(one.tiles[0].column_span, 12);
        assert_eq!(one.tiles[0].row_span, 12);

        let two = default_layout(&["front-door".to_owned(), "driveway".to_owned()]);
        assert_eq!(
            two.tiles
                .iter()
                .map(|tile| (tile.column, tile.row, tile.column_span, tile.row_span))
                .collect::<Vec<_>>(),
            [(1, 1, 6, 12), (7, 1, 6, 12)]
        );
    }

    #[test]
    fn registry_projects_restricted_dashboards_only_to_selected_viewers() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-peek-layout-audience-{}",
            uuid::Uuid::new_v4()
        ));
        let viewer_id = uuid::Uuid::new_v4().to_string();
        let other_id = uuid::Uuid::new_v4().to_string();
        let mut store = RegistryStore::open(
            directory.join("peek-layouts.json"),
            &["front-door".to_owned()],
        )
        .unwrap();
        let mut administrator = store.registry_for_principal("local-administrator", true);
        let mut restricted = administrator.layouts[0].clone();
        restricted.id = "front-entry".to_owned();
        restricted.name = "Front entry".to_owned();
        restricted.audience = LayoutAudience {
            everyone: false,
            credential_ids: vec![viewer_id.clone()],
        };
        administrator.layouts.push(restricted);
        administrator.active_layout_id = "front-entry".to_owned();

        store
            .replace_for("local-administrator", true, 1, administrator)
            .unwrap();

        let administrator = store.registry_for_principal("local-administrator", true);
        let viewer = store.registry_for(&viewer_id);
        let other = store.registry_for(&other_id);
        assert_eq!(administrator.layouts.len(), 2);
        assert!(
            viewer
                .layouts
                .iter()
                .any(|layout| layout.id == "front-entry")
        );
        assert_eq!(other.active_layout_id, DEFAULT_LAYOUT_ID);
        assert_eq!(
            other
                .layouts
                .iter()
                .map(|layout| layout.id.as_str())
                .collect::<Vec<_>>(),
            [DEFAULT_LAYOUT_ID]
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stored_shared_layouts_are_validated_without_user_entries() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-peek-layout-shared-validation-{}",
            uuid::Uuid::new_v4()
        ));
        let path = directory.join("peek-layouts.json");
        let registry = StoredRegistry {
            schema_version: SCHEMA_VERSION,
            revision: 1,
            shared_layouts: vec![Layout {
                id: DEFAULT_LAYOUT_ID.to_owned(),
                name: "Default".to_owned(),
                scope: LayoutScope::Shared,
                owner_id: SHARED_OWNER_ID.to_owned(),
                audience: LayoutAudience::default(),
                activity_focus: true,
                tiles: vec![
                    LayoutTile {
                        camera_id: "front-door".to_owned(),
                        column: 1,
                        row: 1,
                        column_span: 8,
                        row_span: 12,
                        pinned: true,
                    },
                    LayoutTile {
                        camera_id: "driveway".to_owned(),
                        column: 8,
                        row: 1,
                        column_span: 4,
                        row_span: 4,
                        pinned: false,
                    },
                ],
            }],
            users: BTreeMap::new(),
        };
        crate::config::write_private_file_atomically(
            &path,
            &serde_json::to_vec_pretty(&registry).unwrap(),
        )
        .unwrap();

        let error = RegistryStore::open(path, &["front-door".to_owned(), "driveway".to_owned()])
            .err()
            .expect("overlapping stored shared tiles must be rejected");

        assert_eq!(error.to_string(), "layout tiles must not overlap");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stored_default_is_upgraded_to_the_dynamic_all_cameras_dashboard() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-peek-layout-default-upgrade-{}",
            uuid::Uuid::new_v4()
        ));
        let path = directory.join("peek-layouts.json");
        let mut legacy_default = default_layout(&["front-door".to_owned()]);
        legacy_default.name = "Front of house".to_owned();
        let registry = StoredRegistry {
            schema_version: SCHEMA_VERSION,
            revision: 7,
            shared_layouts: vec![legacy_default],
            users: BTreeMap::new(),
        };
        crate::config::write_private_file_atomically(
            &path,
            &serde_json::to_vec_pretty(&registry).unwrap(),
        )
        .unwrap();

        let store = RegistryStore::open(
            path.clone(),
            &["driveway".to_owned(), "front-door".to_owned()],
        )
        .unwrap();
        let upgraded = store.registry_for_principal("local-administrator", true);

        assert_eq!(store.revision(), 8);
        assert_eq!(upgraded.active_layout_id, DEFAULT_LAYOUT_ID);
        assert_eq!(upgraded.layouts[0].name, "All cameras");
        assert_eq!(upgraded.layouts[0].audience, LayoutAudience::default());
        assert_eq!(
            upgraded.layouts[0]
                .tiles
                .iter()
                .map(|tile| tile.camera_id.as_str())
                .collect::<Vec<_>>(),
            ["driveway", "front-door"]
        );
        let persisted: StoredRegistry =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted.revision, 8);
        assert_eq!(persisted.shared_layouts[0].name, "All cameras");

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stored_private_layouts_migrate_to_restricted_server_dashboards() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-peek-layout-private-upgrade-{}",
            uuid::Uuid::new_v4()
        ));
        let path = directory.join("peek-layouts.json");
        let viewer_id = uuid::Uuid::new_v4().to_string();
        let mut private = default_layout(&["front-door".to_owned()]);
        private.id = "night".to_owned();
        private.name = "Night".to_owned();
        private.scope = LayoutScope::Private;
        private.owner_id.clone_from(&viewer_id);
        let registry = StoredRegistry {
            schema_version: SCHEMA_VERSION,
            revision: 4,
            shared_layouts: vec![default_layout(&["front-door".to_owned()])],
            users: BTreeMap::from([(
                viewer_id.clone(),
                StoredUserRegistry {
                    active_layout_id: "night".to_owned(),
                    layouts: vec![private],
                },
            )]),
        };
        crate::config::write_private_file_atomically(
            &path,
            &serde_json::to_vec_pretty(&registry).unwrap(),
        )
        .unwrap();

        let store = RegistryStore::open(path, &["front-door".to_owned()]).unwrap();
        let administrator = store.registry_for_principal("local-administrator", true);
        let viewer = store.registry_for(&viewer_id);
        let other = store.registry_for(&uuid::Uuid::new_v4().to_string());
        let migrated = administrator
            .layouts
            .iter()
            .find(|layout| layout.id == "night")
            .expect("private layout must migrate");

        assert_eq!(store.revision(), 5);
        assert_eq!(migrated.scope, LayoutScope::Shared);
        assert_eq!(migrated.owner_id, SHARED_OWNER_ID);
        assert_eq!(
            migrated.audience,
            LayoutAudience {
                everyone: false,
                credential_ids: vec![viewer_id.clone()],
            }
        );
        assert_eq!(viewer.active_layout_id, "night");
        assert!(viewer.layouts.iter().any(|layout| layout.id == "night"));
        assert!(!other.layouts.iter().any(|layout| layout.id == "night"));
        assert!(store.registry.users[&viewer_id].layouts.is_empty());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn registry_persists_active_dashboard_per_principal_and_rejects_stale_writes() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-peek-layouts-{}", uuid::Uuid::new_v4()));
        let path = directory.join("peek-layouts.json");
        let camera_ids = vec!["front-door".to_owned(), "driveway".to_owned()];
        let mut store = RegistryStore::open(path.clone(), &camera_ids).unwrap();
        let alice_id = uuid::Uuid::new_v4().to_string();
        let mut administrator = store.registry_for_principal("local-administrator", true);
        let mut night = administrator.layouts[0].clone();
        night.id = "night".to_owned();
        night.name = "Perimeter night".to_owned();
        night.audience = LayoutAudience {
            everyone: false,
            credential_ids: vec![alice_id.clone()],
        };
        administrator.layouts.push(night);
        store
            .replace_for("local-administrator", true, 1, administrator)
            .unwrap();

        let mut alice_registry = store.registry_for(&alice_id);
        alice_registry.active_layout_id = "night".to_owned();
        store
            .replace_for(&alice_id, false, 2, alice_registry.clone())
            .unwrap();
        drop(store);

        let mut reopened = RegistryStore::open(path, &camera_ids).unwrap();
        assert_eq!(reopened.revision(), 3);
        assert_eq!(reopened.registry_for(&alice_id), alice_registry);
        let bob_registry = reopened.registry_for(&uuid::Uuid::new_v4().to_string());
        assert_eq!(bob_registry.active_layout_id, "default");
        assert_eq!(bob_registry.layouts.len(), 1);

        let error = reopened
            .replace_for(&alice_id, false, 2, alice_registry)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "layout registry revision conflict (current 3)"
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn retained_dashboard_camera_ids_do_not_authorize_user_mutations() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-peek-layout-private-camera-{}",
            uuid::Uuid::new_v4()
        ));
        let path = directory.join("peek-layouts.json");
        let original_camera_ids = vec!["front-door".to_owned(), "retired-camera".to_owned()];
        let mut store = RegistryStore::open(path.clone(), &original_camera_ids).unwrap();
        let mut administrator = store.registry_for_principal("local-administrator", true);
        let mut retained = administrator.layouts[0].clone();
        retained.id = "retained".to_owned();
        retained.name = "Retained camera".to_owned();
        retained
            .tiles
            .retain(|tile| tile.camera_id == "retired-camera");
        administrator.layouts.push(retained);
        store
            .replace_for("local-administrator", true, 1, administrator)
            .unwrap();
        drop(store);

        let mut reopened = RegistryStore::open(path, &["front-door".to_owned()]).unwrap();
        let alice_id = uuid::Uuid::new_v4().to_string();
        let mut alice = reopened.registry_for(&alice_id);
        let mut guessed = alice.layouts[0].clone();
        guessed.id = "alice-private".to_owned();
        guessed.tiles = vec![LayoutTile {
            camera_id: "retired-camera".to_owned(),
            column: 1,
            row: 1,
            column_span: 12,
            row_span: 12,
            pinned: false,
        }];
        alice.layouts.push(guessed);
        alice.active_layout_id = "alice-private".to_owned();

        let error = reopened
            .replace_for(&alice_id, false, 3, alice)
            .unwrap_err();

        assert!(matches!(error, RegistryError::NotAuthorized { .. }));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn new_principal_uses_all_cameras_as_the_active_fallback() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-peek-layout-shared-fallback-{}",
            uuid::Uuid::new_v4()
        ));
        let path = directory.join("peek-layouts.json");
        let mut store = RegistryStore::open(path, &["front-door".to_owned()]).unwrap();
        let mut administrator = store.registry_for_principal("local-administrator", true);
        let mut shared = administrator.layouts[0].clone();
        shared.id = "shared-alternative".to_owned();
        shared.name = "Shared alternative".to_owned();
        administrator.layouts.push(shared);
        administrator.active_layout_id = "shared-alternative".to_owned();
        store
            .replace_for("local-administrator", true, 1, administrator)
            .unwrap();

        let new_principal = store.registry_for("new-principal");

        assert_eq!(new_principal.active_layout_id, DEFAULT_LAYOUT_ID);
        assert!(
            new_principal
                .layouts
                .iter()
                .any(|layout| layout.id == new_principal.active_layout_id)
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn state_store_get_initializes_a_principal_owned_registry() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-peek-layout-dispatch-{}",
            uuid::Uuid::new_v4()
        ));
        let config_path = directory.join("config.toml");
        let state = ServerState::empty().with_camera_config_path(config_path);
        let principal = ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let command = proto::StateStoreCommand {
            action: Some(state_store_command::Action::Get(proto::GetState {
                namespace: NAMESPACE.to_owned(),
                key: REGISTRY_KEY.to_owned(),
            })),
        };

        let control_ok::Result::StateStoreResult(result) =
            dispatch(&state, &principal, command).unwrap()
        else {
            panic!("Peek layout GetState must return a StateStoreResult");
        };
        let Some(state_store_result::Result::Entry(entry)) = result.result else {
            panic!("Peek layout GetState must return a StateEntry");
        };
        assert_eq!(entry.namespace, NAMESPACE);
        assert_eq!(entry.key, REGISTRY_KEY);
        assert_eq!(entry.schema, REGISTRY_SCHEMA);
        assert_eq!(entry.revision, 1);
        assert_eq!(entry.owner_id, "local-administrator");
        assert!(directory.join("peek-layouts.json").is_file());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn state_store_rejects_shared_layout_changes_from_user_principals() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-peek-layout-auth-{}",
            uuid::Uuid::new_v4()
        ));
        let state = ServerState::empty().with_camera_config_path(directory.join("config.toml"));
        let credential_id = uuid::Uuid::new_v4();
        let principal = ApiPrincipal::credential(AuthenticatedCredential {
            id: credential_id,
            name: "Viewer".to_owned(),
            role: AccessRole::User,
            revision: 1,
            expires_at_ms: None,
        });
        let get = proto::StateStoreCommand {
            action: Some(state_store_command::Action::Get(proto::GetState {
                namespace: NAMESPACE.to_owned(),
                key: REGISTRY_KEY.to_owned(),
            })),
        };
        let control_ok::Result::StateStoreResult(result) =
            dispatch(&state, &principal, get).unwrap()
        else {
            panic!("Peek layout GetState must return a StateStoreResult");
        };
        let Some(state_store_result::Result::Entry(entry)) = result.result else {
            panic!("Peek layout GetState must return a StateEntry");
        };
        let mut registry: LayoutRegistry =
            serde_json::from_value(struct_to_json(entry.value.unwrap())).unwrap();
        registry.layouts[0].name = "Changed by viewer".to_owned();
        let put = proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: NAMESPACE.to_owned(),
                key: REGISTRY_KEY.to_owned(),
                schema: REGISTRY_SCHEMA.to_owned(),
                value: Some(json_to_struct(serde_json::to_value(registry).unwrap()).unwrap()),
                expected_revision: Some(entry.revision),
                ttl: None,
            })),
        };

        let error = dispatch(&state, &principal, put).unwrap_err();

        assert_eq!(error.code, proto::ErrorCode::Rejected);
        assert_eq!(error.details.len(), 1);
        let detail = proto::StateStoreError::decode(error.details[0].value.as_slice()).unwrap();
        assert_eq!(
            detail.code,
            proto::StateStoreErrorCode::NotAuthorized as i32
        );
        assert_eq!(detail.current_revision, Some(1));
        assert_eq!(detail.namespace, NAMESPACE);
        assert_eq!(detail.key, REGISTRY_KEY);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn state_store_rejects_unknown_dashboard_viewer_identities() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-peek-layout-viewer-auth-{}",
            uuid::Uuid::new_v4()
        ));
        let state = ServerState::empty().with_camera_config_path(directory.join("config.toml"));
        let principal = ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let get = proto::StateStoreCommand {
            action: Some(state_store_command::Action::Get(proto::GetState {
                namespace: NAMESPACE.to_owned(),
                key: REGISTRY_KEY.to_owned(),
            })),
        };
        let control_ok::Result::StateStoreResult(result) =
            dispatch(&state, &principal, get).unwrap()
        else {
            panic!("Peek layout GetState must return a StateStoreResult");
        };
        let Some(state_store_result::Result::Entry(entry)) = result.result else {
            panic!("Peek layout GetState must return a StateEntry");
        };
        let mut registry: LayoutRegistry =
            serde_json::from_value(struct_to_json(entry.value.unwrap())).unwrap();
        let mut restricted = registry.layouts[0].clone();
        restricted.id = "restricted".to_owned();
        restricted.name = "Restricted".to_owned();
        restricted.audience = LayoutAudience {
            everyone: false,
            credential_ids: vec![uuid::Uuid::new_v4().to_string()],
        };
        registry.layouts.push(restricted);
        registry.active_layout_id = "restricted".to_owned();
        let put = proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: NAMESPACE.to_owned(),
                key: REGISTRY_KEY.to_owned(),
                schema: REGISTRY_SCHEMA.to_owned(),
                value: Some(json_to_struct(serde_json::to_value(registry).unwrap()).unwrap()),
                expected_revision: Some(entry.revision),
                ttl: None,
            })),
        };

        let error = dispatch(&state, &principal, put).unwrap_err();

        assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
        assert_eq!(error.message, "dashboard viewer identity does not exist");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn state_store_put_persists_and_returns_a_typed_revision_conflict() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-peek-layout-put-{}", uuid::Uuid::new_v4()));
        let state = ServerState::empty().with_camera_config_path(directory.join("config.toml"));
        let principal = ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let get = proto::StateStoreCommand {
            action: Some(state_store_command::Action::Get(proto::GetState {
                namespace: NAMESPACE.to_owned(),
                key: REGISTRY_KEY.to_owned(),
            })),
        };
        let control_ok::Result::StateStoreResult(result) =
            dispatch(&state, &principal, get.clone()).unwrap()
        else {
            panic!("Peek layout GetState must return a StateStoreResult");
        };
        let Some(state_store_result::Result::Entry(entry)) = result.result else {
            panic!("Peek layout GetState must return a StateEntry");
        };
        let mut registry: LayoutRegistry =
            serde_json::from_value(struct_to_json(entry.value.unwrap())).unwrap();
        let mut dashboard = registry.layouts[0].clone();
        dashboard.id = "front-entry".to_owned();
        dashboard.name = "Front entry".to_owned();
        registry.layouts.push(dashboard);
        registry.active_layout_id = "front-entry".to_owned();
        let put = proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: NAMESPACE.to_owned(),
                key: REGISTRY_KEY.to_owned(),
                schema: REGISTRY_SCHEMA.to_owned(),
                value: Some(json_to_struct(serde_json::to_value(registry).unwrap()).unwrap()),
                expected_revision: Some(1),
                ttl: None,
            })),
        };

        let control_ok::Result::StateStoreResult(saved) =
            dispatch(&state, &principal, put.clone()).unwrap()
        else {
            panic!("Peek layout PutState must return a StateStoreResult");
        };
        let Some(state_store_result::Result::Entry(saved)) = saved.result else {
            panic!("Peek layout PutState must return a StateEntry");
        };
        assert_eq!(saved.revision, 2);
        let control_ok::Result::StateStoreResult(reloaded) =
            dispatch(&state, &principal, get).unwrap()
        else {
            panic!("Peek layout GetState must return a StateStoreResult");
        };
        let Some(state_store_result::Result::Entry(reloaded)) = reloaded.result else {
            panic!("Peek layout GetState must return a StateEntry");
        };
        let reloaded: LayoutRegistry =
            serde_json::from_value(struct_to_json(reloaded.value.unwrap())).unwrap();
        assert_eq!(reloaded.active_layout_id, "front-entry");
        assert_eq!(reloaded.layouts[1].name, "Front entry");

        let error = dispatch(&state, &principal, put).unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected);
        let detail = proto::StateStoreError::decode(error.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.code, proto::StateStoreErrorCode::Conflict as i32);
        assert_eq!(detail.current_revision, Some(2));

        std::fs::remove_dir_all(directory).unwrap();
    }
}
