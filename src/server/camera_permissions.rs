use super::{ApiPrincipal, ControlCommandError, ServerControlHandler};
use crate::{
    access::{AccessRole, CameraAccess, CameraAccessConflict, MAX_CAMERA_ACCESS_IDS},
    api::proto::{self, ok as control_ok, state_store_command, state_store_result},
    webrtc::ControlRequestHandler as _,
};
use prost::Message as _;
use prost_types::{ListValue, Struct, Value, value::Kind};
use uuid::Uuid;

pub(super) const NAMESPACE: &str = "keeppeek.camera-access";
pub(super) const CAPABILITY_ID: &str = "keeppeek.camera-access.v1";

pub(super) fn handles(command: &proto::StateStoreCommand) -> bool {
    match &command.action {
        Some(state_store_command::Action::Get(request)) => request.namespace == NAMESPACE,
        Some(state_store_command::Action::Put(request)) => request.namespace == NAMESPACE,
        Some(state_store_command::Action::Delete(request)) => request.namespace == NAMESPACE,
        Some(state_store_command::Action::Watch(request)) => request.namespace == NAMESPACE,
        _ => false,
    }
}

pub(super) fn dispatch(
    handler: &ServerControlHandler,
    principal: &ApiPrincipal,
    command: proto::StateStoreCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    if principal.role != AccessRole::Administrator {
        return Err(super::camera_access::denied());
    }
    let group_ids = available_groups(handler)?;
    let credential_id = match command.action {
        Some(state_store_command::Action::Get(request)) => {
            parse_target(&request.namespace, &request.key)?
        }
        Some(state_store_command::Action::Put(request)) => put(handler, request)?,
        _ => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::UnsupportedRequest,
                501,
                "camera access supports only GetState and PutState",
            ));
        }
    };
    entry(handler, credential_id, group_ids).map(control_ok::Result::StateStoreResult)
}

fn parse_target(namespace: &str, key: &str) -> Result<Uuid, ControlCommandError> {
    if namespace != NAMESPACE {
        return Err(invalid("camera access namespace is invalid"));
    }
    let id = Uuid::parse_str(key).map_err(|_| invalid("credential identity is invalid"))?;
    if id.to_string() != key {
        return Err(invalid("credential identity must be a canonical UUID"));
    }
    Ok(id)
}

fn put(
    handler: &ServerControlHandler,
    request: proto::PutState,
) -> Result<Uuid, ControlCommandError> {
    let id = parse_target(&request.namespace, &request.key)?;
    if request.schema != CAPABILITY_ID || request.ttl.is_some() {
        return Err(invalid("camera access schema or TTL is invalid"));
    }
    let expected_revision = request
        .expected_revision
        .ok_or_else(|| invalid("credential revision is required"))?;
    let policy = parse_policy(
        request
            .value
            .ok_or_else(|| invalid("camera access policy is required"))?,
    )?;
    let cameras = handler.state.camera_entries();
    if policy
        .camera_ids
        .iter()
        .any(|id| !cameras.iter().any(|camera| camera.info.id == *id))
    {
        return Err(invalid("camera access contains an unknown camera"));
    }
    if policy
        .group_ids
        .iter()
        .any(|group| !cameras.iter().any(|camera| camera.groups.contains(group)))
    {
        return Err(invalid("user access contains an unknown camera group"));
    }
    handler
        .state
        .access_manager
        .set_camera_access(id, expected_revision, policy)
        .map_err(|error| mutation_error(error, &request.key))?;
    for session_id in handler.remove_credential_sessions(id) {
        handler.session_closed(session_id);
        handler.state.webrtc.request_api_session_close(session_id);
    }
    Ok(id)
}

fn parse_policy(value: Struct) -> Result<CameraAccess, ControlCommandError> {
    if value
        .fields
        .keys()
        .any(|key| !matches!(key.as_str(), "all_cameras" | "group_ids" | "camera_ids"))
    {
        return Err(invalid(
            "user access must contain only all_cameras, group_ids, and camera_ids",
        ));
    }
    let Some(Kind::BoolValue(all_cameras)) = value
        .fields
        .get("all_cameras")
        .and_then(|value| value.kind.as_ref())
    else {
        return Err(invalid("all_cameras must be a boolean"));
    };
    let policy = CameraAccess {
        all_cameras: *all_cameras,
        group_ids: value
            .fields
            .get("group_ids")
            .map(parse_ids)
            .transpose()?
            .unwrap_or_default(),
        camera_ids: parse_ids(
            value
                .fields
                .get("camera_ids")
                .ok_or_else(|| invalid("camera_ids is required"))?,
        )?,
    };
    policy
        .validate()
        .map_err(|_| invalid("camera access IDs are invalid or duplicated"))?;
    Ok(policy)
}

fn parse_ids(value: &Value) -> Result<Vec<String>, ControlCommandError> {
    let Some(Kind::ListValue(ids)) = &value.kind else {
        return Err(invalid("user access IDs must be arrays"));
    };
    if ids.values.len() > MAX_CAMERA_ACCESS_IDS {
        return Err(invalid("user access exceeds its group or camera limit"));
    }
    ids.values
        .iter()
        .map(|value| match &value.kind {
            Some(Kind::StringValue(id)) => Ok(id.clone()),
            _ => Err(invalid("user access IDs must be strings")),
        })
        .collect()
}

fn string_list(ids: Vec<String>) -> Value {
    Value {
        kind: Some(Kind::ListValue(ListValue {
            values: ids
                .into_iter()
                .map(|id| Value {
                    kind: Some(Kind::StringValue(id)),
                })
                .collect(),
        })),
    }
}

fn entry(
    handler: &ServerControlHandler,
    id: Uuid,
    group_ids: Vec<String>,
) -> Result<proto::StateStoreResult, ControlCommandError> {
    let (policy, metadata) = handler
        .state
        .access_manager
        .camera_access_settings(id)
        .ok_or_else(|| {
            ControlCommandError::new(proto::ErrorCode::NotFound, 404, "credential was not found")
        })?;
    let value = Struct {
        fields: [
            (
                "all_cameras".to_owned(),
                Value {
                    kind: Some(Kind::BoolValue(policy.all_cameras)),
                },
            ),
            ("group_ids".to_owned(), string_list(policy.group_ids)),
            ("camera_ids".to_owned(), string_list(policy.camera_ids)),
            ("available_group_ids".to_owned(), string_list(group_ids)),
        ]
        .into_iter()
        .collect(),
    };
    Ok(proto::StateStoreResult {
        result: Some(state_store_result::Result::Entry(proto::StateEntry {
            namespace: NAMESPACE.to_owned(),
            key: id.to_string(),
            schema: CAPABILITY_ID.to_owned(),
            value: Some(value),
            revision: metadata.revision,
            updated_at: None,
            expires_at: None,
            owner_id: id.to_string(),
        })),
    })
}

fn available_groups(handler: &ServerControlHandler) -> Result<Vec<String>, ControlCommandError> {
    let group_ids = handler
        .state
        .camera_entries()
        .into_iter()
        .flat_map(|camera| camera.groups)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let inventory = CameraAccess {
        group_ids,
        ..Default::default()
    };
    inventory.validate().map_err(|_| {
        invalid("available camera groups exceed the user access ID or count limits")
    })?;
    Ok(inventory.group_ids)
}

fn mutation_error(error: anyhow::Error, key: &str) -> ControlCommandError {
    if let Some(conflict) = error.downcast_ref::<CameraAccessConflict>() {
        return ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "camera access changed; reload before saving",
        )
        .with_detail(prost_types::Any {
            type_url: "type.googleapis.com/keeppeek.webrtc.v1.StateStoreError".to_owned(),
            value: proto::StateStoreError {
                namespace: NAMESPACE.to_owned(),
                key: key.to_owned(),
                code: proto::StateStoreErrorCode::Conflict as i32,
                current_revision: Some(conflict.current_revision),
            }
            .encode_to_vec(),
        });
    }
    tracing::warn!(
        event = "camera_access_save_failed",
        "camera access update was rejected or could not be persisted"
    );
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        400,
        "camera access could not be saved",
    )
}

fn invalid(message: &'static str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_access_accepts_group_and_camera_lists_without_broadening_legacy_policies() {
        let strings = |ids: &[&str]| Value {
            kind: Some(Kind::ListValue(ListValue {
                values: ids
                    .iter()
                    .map(|id| Value {
                        kind: Some(Kind::StringValue((*id).to_owned())),
                    })
                    .collect(),
            })),
        };
        let value = Struct {
            fields: [
                (
                    "all_cameras".to_owned(),
                    Value {
                        kind: Some(Kind::BoolValue(false)),
                    },
                ),
                ("group_ids".to_owned(), strings(&["outdoor"])),
                ("camera_ids".to_owned(), strings(&["192.0.2.10"])),
            ]
            .into_iter()
            .collect(),
        };
        let policy = parse_policy(value.clone()).expect("per-user access must accept group IDs");
        assert_eq!(policy.group_ids, ["outdoor"]);
        assert_eq!(policy.camera_ids, ["192.0.2.10"]);
        let mut legacy = value.clone();
        legacy.fields.remove("group_ids");
        assert!(parse_policy(legacy).unwrap().group_ids.is_empty());
        let mut invalid = value;
        invalid.fields.insert(
            "all_cameras".to_owned(),
            Value {
                kind: Some(Kind::BoolValue(true)),
            },
        );
        assert!(parse_policy(invalid).is_err());
    }
}
