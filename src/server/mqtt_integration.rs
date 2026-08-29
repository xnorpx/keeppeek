use super::{ControlCommandError, ServerState};
use crate::{
    api::proto::{self, ok as control_ok, state_store_command, state_store_result},
    config::{self, MqttPasswordUpdate},
    event_forwarder::{BrokerFailureKind, MqttStatus, config::MqttForwarderConfig},
};
use prost_types::{ListValue, Struct, Timestamp, Value, value::Kind};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::SystemTime};

const NAMESPACE: &str = "keeppeek.integrations.mqtt";
const CONFIGURATION_KEY: &str = "configuration";
const TEST_KEY: &str = "test";
const CONFIGURATION_SCHEMA: &str = "keeppeek.mqtt-configuration.v1";
const TEST_SCHEMA: &str = "keeppeek.mqtt-test.v1";
const TEST_RESULT_SCHEMA: &str = "keeppeek.mqtt-test-result.v1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MqttSettingsRequest {
    enabled: bool,
    broker_url: String,
    client_id: String,
    instance_id: String,
    forwarder_id: String,
    topic_prefix: String,
    username: Option<String>,
    password: Option<String>,
    #[serde(default)]
    clear_password: bool,
    tls_ca_path: Option<String>,
    qos: f64,
    retain_events: bool,
    retain_health: bool,
    outbox_max_mb: f64,
    retry_min_ms: f64,
    retry_max_ms: f64,
}

#[derive(Debug, Serialize)]
struct MqttSettingsResponse {
    configuration: SanitizedMqttConfiguration,
    status: MqttStatus,
}

#[derive(Debug, Serialize)]
struct SanitizedMqttConfiguration {
    enabled: bool,
    broker_url: String,
    client_id: String,
    instance_id: String,
    forwarder_id: String,
    topic_prefix: String,
    username: Option<String>,
    password_configured: bool,
    tls_ca_path: Option<String>,
    qos: u8,
    retain_events: bool,
    retain_health: bool,
    outbox_max_mb: u64,
    retry_min_ms: u64,
    retry_max_ms: u64,
}

#[derive(Debug, Serialize)]
struct MqttTestResponse {
    ok: bool,
    kind: Option<BrokerFailureKind>,
    detail: String,
}

pub(super) fn dispatch(
    state: &ServerState,
    command: proto::StateStoreCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    let result = match command.action {
        Some(state_store_command::Action::Get(request)) => get(state, request)?,
        Some(state_store_command::Action::Put(request)) => put(state, request)?,
        Some(
            state_store_command::Action::Delete(_)
            | state_store_command::Action::Watch(_)
            | state_store_command::Action::Unwatch(_),
        ) => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::UnsupportedRequest,
                501,
                "this StateStore operation is not implemented by the server",
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

fn get(
    state: &ServerState,
    request: proto::GetState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    require_target(&request.namespace, &request.key, CONFIGURATION_KEY)?;
    let handle = state.event_forwarder.as_ref().ok_or_else(unavailable)?;
    state_entry(
        CONFIGURATION_KEY,
        CONFIGURATION_SCHEMA,
        handle.revision(),
        &settings_response(handle.config(), handle.status()),
    )
}

fn put(
    state: &ServerState,
    request: proto::PutState,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    if request.namespace != NAMESPACE {
        return Err(invalid("MQTT integration namespace is invalid"));
    }
    let value = request
        .value
        .ok_or_else(|| invalid("MQTT state value is required"))?;
    let submitted: MqttSettingsRequest = serde_json::from_value(struct_to_json(value))
        .map_err(|_| invalid("MQTT state value is invalid"))?;
    let handle = state.event_forwarder.as_ref().ok_or_else(unavailable)?;
    match (request.key.as_str(), request.schema.as_str()) {
        (CONFIGURATION_KEY, CONFIGURATION_SCHEMA) => {
            if request
                .expected_revision
                .is_some_and(|expected| expected != handle.revision())
            {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    409,
                    "MQTT configuration changed after this editor was opened; reload before applying the draft",
                ));
            }
            let Some(config_path) = &state.camera_config_path else {
                return Err(unavailable());
            };
            let _update = state
                .config_update
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (next, password_update) = requested_config(submitted, &handle.config())?;
            let saved = config::update_mqtt_forwarder(config_path, next, password_update)
                .map_err(|error| invalid(&error.to_string()))?;
            handle
                .update_config(saved.event_forwarder.mqtt.clone())
                .map_err(|error| internal(&error.to_string()))?;
            state_entry(
                CONFIGURATION_KEY,
                CONFIGURATION_SCHEMA,
                handle.revision(),
                &settings_response(saved.event_forwarder.mqtt, handle.status()),
            )
        }
        (TEST_KEY, TEST_SCHEMA) => {
            let (candidate, _) = requested_config(submitted, &handle.config())?;
            let result = match handle.test_config(&candidate) {
                Ok(()) => MqttTestResponse {
                    ok: true,
                    kind: None,
                    detail: "Connected and published a test status to the MQTT 5 broker."
                        .to_owned(),
                },
                Err(error) => MqttTestResponse {
                    ok: false,
                    kind: Some(error.kind),
                    detail: error.detail,
                },
            };
            state_entry(TEST_KEY, TEST_RESULT_SCHEMA, handle.revision(), &result)
        }
        _ => Err(invalid("MQTT state key or schema is invalid")),
    }
}

fn require_target(
    namespace: &str,
    key: &str,
    expected_key: &str,
) -> Result<(), ControlCommandError> {
    if namespace != NAMESPACE || key != expected_key {
        return Err(invalid("MQTT state key or namespace is invalid"));
    }
    Ok(())
}

fn requested_config(
    request: MqttSettingsRequest,
    current: &MqttForwarderConfig,
) -> Result<(MqttForwarderConfig, MqttPasswordUpdate), ControlCommandError> {
    if request.clear_password && request.password.is_some() {
        return Err(invalid(
            "MQTT password cannot be replaced and cleared in the same request",
        ));
    }
    let username = request
        .username
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let mut password_update = match request.password {
        Some(password) => MqttPasswordUpdate::Set(password),
        None if request.clear_password => MqttPasswordUpdate::Clear,
        None => MqttPasswordUpdate::Preserve,
    };
    let password = if username.is_none() {
        password_update = MqttPasswordUpdate::Clear;
        None
    } else {
        match &password_update {
            MqttPasswordUpdate::Set(password) => Some(password.clone()),
            MqttPasswordUpdate::Clear => None,
            MqttPasswordUpdate::Preserve => current.password.clone(),
        }
    };
    let config = MqttForwarderConfig {
        revision: current.revision,
        enabled: request.enabled,
        broker_url: request.broker_url.trim().to_owned(),
        client_id: request.client_id.trim().to_owned(),
        instance_id: request.instance_id.trim().to_owned(),
        forwarder_id: request.forwarder_id.trim().to_owned(),
        topic_prefix: request.topic_prefix.trim().to_owned(),
        username,
        password,
        tls_ca_path: request
            .tls_ca_path
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        qos: u8::try_from(whole_number(request.qos, "MQTT QoS")?)
            .map_err(|_| invalid("MQTT QoS is too large"))?,
        retain_events: request.retain_events,
        retain_health: request.retain_health,
        outbox_max_mb: whole_number(request.outbox_max_mb, "MQTT outbox limit")?,
        retry_min_ms: whole_number(request.retry_min_ms, "MQTT retry minimum")?,
        retry_max_ms: whole_number(request.retry_max_ms, "MQTT retry maximum")?,
    };
    config
        .validate()
        .map_err(|error| invalid(&error.to_string()))?;
    Ok((config, password_update))
}

fn whole_number(value: f64, label: &str) -> Result<u64, ControlCommandError> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > u64::MAX as f64 {
        return Err(invalid(&format!(
            "{label} must be a nonnegative whole number"
        )));
    }
    Ok(value as u64)
}

fn settings_response(config: MqttForwarderConfig, status: MqttStatus) -> MqttSettingsResponse {
    MqttSettingsResponse {
        configuration: SanitizedMqttConfiguration {
            enabled: config.enabled,
            broker_url: config.broker_url,
            client_id: config.client_id,
            instance_id: config.instance_id,
            forwarder_id: config.forwarder_id,
            topic_prefix: config.topic_prefix,
            username: config.username,
            password_configured: config.password.is_some(),
            tls_ca_path: config
                .tls_ca_path
                .map(|path| path.to_string_lossy().into_owned()),
            qos: config.qos,
            retain_events: config.retain_events,
            retain_health: config.retain_health,
            outbox_max_mb: config.outbox_max_mb,
            retry_min_ms: config.retry_min_ms,
            retry_max_ms: config.retry_max_ms,
        },
        status,
    }
}

fn state_entry(
    key: &str,
    schema: &str,
    revision: u64,
    value: &impl Serialize,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let value =
        serde_json::to_value(value).map_err(|_| internal("MQTT state could not be encoded"))?;
    let value = json_to_struct(value).map_err(|_| internal("MQTT state could not be encoded"))?;
    Ok(proto::StateStoreResult {
        result: Some(state_store_result::Result::Entry(proto::StateEntry {
            namespace: NAMESPACE.to_owned(),
            key: key.to_owned(),
            schema: schema.to_owned(),
            value: Some(value),
            revision,
            updated_at: Some(now_timestamp()),
            expires_at: None,
            owner_id: "server".to_owned(),
        })),
    })
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

fn unavailable() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        503,
        "MQTT 5 event forwarder is unavailable",
    )
}

fn internal(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Internal, 500, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{event_forwarder::Runtime, shutdown::Shutdown};

    #[test]
    fn state_response_contains_no_password() {
        let config = MqttForwarderConfig {
            username: Some("operator".to_owned()),
            password: Some("super-secret".to_owned()),
            ..MqttForwarderConfig::default()
        };
        let status = MqttStatus {
            enabled: false,
            state: crate::event_forwarder::MqttConnectionState::Disabled,
            detail: "MQTT 5 event forwarding is disabled.".to_owned(),
            connected_at_ms: None,
            last_received_at_ms: None,
            last_delivered_at_ms: None,
            pending_items: 0,
            pending_bytes: 0,
            oldest_unacknowledged_timestamp_ms: None,
            retry_count: 0,
            duplicate_count: 0,
            outbox_limit_bytes: 64 * 1_024 * 1_024,
        };
        let value = serde_json::to_string(&settings_response(config, status)).unwrap();
        assert!(value.contains("\"password_configured\":true"));
        assert!(!value.contains("super-secret"));
    }

    #[test]
    fn state_store_update_persists_revision_and_hot_disables_runtime() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-mqtt-state-store-{}",
            uuid::Uuid::new_v4()
        ));
        let config_path = directory.join("config.toml");
        config::write_private_file(&config_path, b"host = \"127.0.0.1\"\n").unwrap();
        let shutdown = Shutdown::new();
        let runtime = Runtime::open(
            MqttForwarderConfig::default(),
            &directory.join("mqtt-forwarder.db"),
            shutdown.clone(),
        )
        .unwrap();
        let handle = runtime.handle();
        let state = ServerState::empty()
            .with_camera_config_path(config_path.clone())
            .with_event_forwarder(handle.clone());
        let request = proto::PutState {
            namespace: NAMESPACE.to_owned(),
            key: CONFIGURATION_KEY.to_owned(),
            schema: CONFIGURATION_SCHEMA.to_owned(),
            value: Some(
                json_to_struct(serde_json::json!({
                    "enabled": false,
                    "broker_url": "mqtt://127.0.0.1:1883",
                    "client_id": "keeppeek",
                    "instance_id": "home-nvr",
                    "forwarder_id": "mqtt",
                    "topic_prefix": "keeppeek",
                    "username": "operator",
                    "password": "state-store-secret",
                    "clear_password": false,
                    "tls_ca_path": null,
                    "qos": 1,
                    "retain_events": false,
                    "retain_health": true,
                    "outbox_max_mb": 64,
                    "retry_min_ms": 250,
                    "retry_max_ms": 30000
                }))
                .unwrap(),
            ),
            expected_revision: Some(1),
            ttl: None,
        };

        let result = put(&state, request.clone()).unwrap();
        let Some(state_store_result::Result::Entry(entry)) = result.result else {
            panic!("MQTT update must return a StateEntry");
        };
        assert_eq!(entry.revision, 2);
        assert_eq!(entry.schema, CONFIGURATION_SCHEMA);
        assert_eq!(handle.revision(), 2);
        assert_eq!(
            handle.status().state,
            crate::event_forwarder::MqttConnectionState::Disabled
        );
        assert_eq!(
            config::load_config(&config_path)
                .unwrap()
                .event_forwarder
                .mqtt
                .revision,
            2
        );
        assert!(!format!("{entry:?}").contains("state-store-secret"));

        let error = put(&state, request).unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected);
        assert_eq!(error._http_status, 409);

        shutdown.cancel();
        runtime.join();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
