use super::{ApiPrincipal, ControlCommandError, ServerState, camera_access, event_search_catalog};
use crate::api::proto;
use crate::storage::catalog::workflow;
use prost::Message as _;

pub(super) fn dispatch(
    state: &ServerState,
    principal: &ApiPrincipal,
    command: proto::EventWorkflowCommand,
) -> Result<proto::ok::Result, ControlCommandError> {
    let actor = actor_id(principal, &command.local_workspace_id)?;
    if !command.expected_actor_id.is_empty() && command.expected_actor_id != actor {
        return Err(invalid(
            "reviewer identity changed; reload event state before retrying",
        ));
    }
    let policy = camera_access::for_principal(state, principal)?;
    match command.action {
        Some(proto::event_workflow_command::Action::Get(request)) => {
            get(state, principal, &actor, &policy, request)
        }
        Some(proto::event_workflow_command::Action::Review(request)) => {
            review(state, principal, &actor, &policy, request)
        }
        Some(proto::event_workflow_command::Action::Bookmark(request)) => {
            bookmark(state, principal, &actor, &policy, request)
        }
        Some(proto::event_workflow_command::Action::ListBookmarks(request)) => {
            list_bookmarks(state, principal, &actor, &policy, &request)
        }
        None => Err(invalid("event workflow action is required")),
    }
}

fn get(
    state: &ServerState,
    principal: &ApiPrincipal,
    actor: &str,
    policy: &crate::access::CameraAccess,
    request: proto::GetEventWorkflow,
) -> Result<proto::ok::Result, ControlCommandError> {
    if request.targets.is_empty()
        || request.targets.len() > 16
        || (request.include_audit && request.targets.len() != 1)
    {
        return Err(invalid(
            "read 1 to 16 event states, or exactly one with bookmark audit",
        ));
    }
    let keys = request
        .targets
        .into_iter()
        .map(|target| authorize_target(policy, target))
        .collect::<Result<Vec<_>, _>>()?;
    let states = event_search_catalog(state)?
        .event_workflow(actor, keys)
        .map_err(command_error)?;
    finish(state, principal, actor, states, request.include_audit, true)
}

fn review(
    state: &ServerState,
    principal: &ApiPrincipal,
    actor: &str,
    policy: &crate::access::CameraAccess,
    request: proto::MutateEventReviews,
) -> Result<proto::ok::Result, ControlCommandError> {
    validate_batch(request.changes.len())?;
    let changes = request
        .changes
        .into_iter()
        .map(|change| {
            let target = change
                .target
                .ok_or_else(|| invalid("event target is required"))?;
            Ok(workflow::ReviewChange {
                key: authorize_target(policy, target)?,
                expected_revision: change.expected_revision,
                reviewed: change.reviewed,
                dismissed: change.dismissed,
            })
        })
        .collect::<Result<Vec<_>, ControlCommandError>>()?;
    let states = event_search_catalog(state)?
        .mutate_event_reviews(actor, changes)
        .map_err(command_error)?;
    finish(state, principal, actor, states, false, false)
}

fn bookmark(
    state: &ServerState,
    principal: &ApiPrincipal,
    actor: &str,
    policy: &crate::access::CameraAccess,
    request: proto::EventBookmarkChange,
) -> Result<proto::ok::Result, ControlCommandError> {
    let target = request
        .target
        .ok_or_else(|| invalid("bookmark target is required"))?;
    let states = event_search_catalog(state)?
        .mutate_event_bookmark(
            actor,
            principal.role == crate::access::AccessRole::Administrator,
            workflow::BookmarkChange {
                key: authorize_target(policy, target)?,
                expected_revision: request.expected_revision,
                active: request.active,
                note: request.note,
            },
        )
        .map_err(command_error)?;
    finish(state, principal, actor, states, true, true)
}

fn finish(
    state: &ServerState,
    principal: &ApiPrincipal,
    actor: &str,
    states: Vec<workflow::State>,
    include_audit: bool,
    bookmarks_included: bool,
) -> Result<proto::ok::Result, ControlCommandError> {
    bounded_result(proto::EventWorkflowResult {
        states: states
            .into_iter()
            .map(|mut item| {
                let source_available = state.camera(&item.key.source_id).is_some();
                if !bookmarks_included {
                    item.bookmark = None;
                }
                let mut result = proto_state(item, include_audit);
                result.source_available = source_available;
                result
            })
            .collect(),
        actor_id: actor.to_owned(),
        local_workspace: principal.is_local(),
        bookmarks_included,
        total: None,
        next_page_token: String::new(),
    })
}

fn list_bookmarks(
    state: &ServerState,
    principal: &ApiPrincipal,
    actor: &str,
    policy: &crate::access::CameraAccess,
    request: &proto::ListEventBookmarks,
) -> Result<proto::ok::Result, ControlCommandError> {
    camera_access::require_cameras(policy, &request.source_ids)?;
    let source_ids = if request.source_ids.is_empty() && !policy.all_cameras {
        policy.camera_ids.clone()
    } else {
        request.source_ids.clone()
    };
    let page = event_search_catalog(state)?
        .list_event_bookmarks(workflow::BookmarkQuery {
            actor_id: actor.to_owned(),
            source_ids,
            all_sources: policy.all_cameras,
            start_ms: request.start_ms,
            end_ms: request.end_ms,
            by_me: request.by_me,
            page_size: if request.page_size == 0 {
                16
            } else {
                request.page_size
            },
            page_token: if request.page_token.is_empty() {
                String::new()
            } else {
                super::open_event_page_token(state, &request.page_token)?
            },
        })
        .map_err(command_error)?;
    let next_page_token = if page.next_page_token.is_empty() {
        String::new()
    } else {
        super::seal_event_page_token(state, page.next_page_token)?
    };
    bounded_result(proto::EventWorkflowResult {
        states: page
            .states
            .into_iter()
            .map(|item| {
                let source_available = state.camera(&item.key.source_id).is_some();
                let mut result = proto_state(item, false);
                result.source_available = source_available;
                result
            })
            .collect(),
        actor_id: actor.to_owned(),
        local_workspace: principal.is_local(),
        bookmarks_included: true,
        total: Some(page.total),
        next_page_token,
    })
}

fn bounded_result(
    result: proto::EventWorkflowResult,
) -> Result<proto::ok::Result, ControlCommandError> {
    if result.encoded_len() > crate::webrtc::MAX_CONTROL_MESSAGE_BYTES - 1_024 {
        return Err(invalid(
            "event workflow response exceeds its bound; request fewer events",
        ));
    }
    Ok(proto::ok::Result::EventWorkflowResult(result))
}

fn authorize_target(
    policy: &crate::access::CameraAccess,
    target: proto::EventWorkflowTarget,
) -> Result<workflow::EventKey, ControlCommandError> {
    camera_access::require_camera(policy, &target.source_id)?;
    Ok(workflow::EventKey {
        source_id: target.source_id,
        event_id: target.event_id,
    })
}

fn validate_batch(count: usize) -> Result<(), ControlCommandError> {
    if count == 0 || count > workflow::MAX_BATCH {
        return Err(invalid("event workflow requires 1 to 128 explicit targets"));
    }
    Ok(())
}

fn invalid(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

pub(super) fn proto_state(
    state: workflow::State,
    include_audit: bool,
) -> proto::EventWorkflowState {
    proto::EventWorkflowState {
        target: Some(proto::EventWorkflowTarget {
            source_id: state.key.source_id,
            event_id: state.key.event_id,
        }),
        reviewed: state.reviewed,
        dismissed: state.dismissed,
        review_revision: state.review_revision,
        reviewed_at_ms: state.reviewed_at_ms,
        dismissed_at_ms: state.dismissed_at_ms,
        updated_at_ms: state.updated_at_ms,
        bookmark: state.bookmark.map(|bookmark| proto::EventBookmark {
            active: bookmark.active,
            note: bookmark.note,
            revision: bookmark.revision,
            created_by: bookmark.created_by,
            created_at_ms: bookmark.created_at_ms,
            updated_by: bookmark.updated_by,
            updated_at_ms: bookmark.updated_at_ms,
            event_start_ms: bookmark.event_start_ms,
            event_kind: bookmark.event_kind,
            audit: if include_audit {
                bookmark
                    .audit
                    .into_iter()
                    .map(|entry| proto::EventBookmarkAudit {
                        revision: entry.revision,
                        actor_id: entry.actor_id,
                        occurred_at_ms: entry.occurred_at_ms,
                        action: entry.action,
                    })
                    .collect()
            } else {
                Vec::new()
            },
        }),
        event_present: state.event_present,
        media_available: state.media_available,
        source_available: true,
    }
}

fn command_error(error: anyhow::Error) -> ControlCommandError {
    if let Some(conflict) = error.downcast_ref::<workflow::Conflict>() {
        let detail = proto::EventWorkflowError {
            code: proto::EventWorkflowErrorCode::Conflict as i32,
            current: Some(proto_state(conflict.current.clone(), false)),
        };
        return ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "event workflow changed; reload current state and retry your action",
        )
        .with_detail(prost_types::Any {
            type_url: "type.googleapis.com/keeppeek.webrtc.v1.EventWorkflowError".to_owned(),
            value: detail.encode_to_vec(),
        });
    }
    let failure = error
        .downcast_ref::<workflow::Failure>()
        .unwrap_or(&workflow::Failure::Unavailable);
    let (code, http_status, detail_code) = match failure {
        workflow::Failure::Invalid(_) => (
            proto::ErrorCode::InvalidRequest,
            400,
            proto::EventWorkflowErrorCode::Invalid,
        ),
        workflow::Failure::Limit(_) => (
            proto::ErrorCode::Rejected,
            429,
            proto::EventWorkflowErrorCode::Limit,
        ),
        workflow::Failure::Forbidden => (
            proto::ErrorCode::Rejected,
            403,
            proto::EventWorkflowErrorCode::NotAuthorized,
        ),
        workflow::Failure::NotFound => (
            proto::ErrorCode::NotFound,
            404,
            proto::EventWorkflowErrorCode::Invalid,
        ),
        workflow::Failure::Unavailable => (
            proto::ErrorCode::Unavailable,
            503,
            proto::EventWorkflowErrorCode::Unavailable,
        ),
    };
    tracing::warn!(name: "event.workflow.failure", error_code = ?detail_code, "event workflow command failed");
    ControlCommandError::new(code, http_status, failure.message()).with_detail(prost_types::Any {
        type_url: "type.googleapis.com/keeppeek.webrtc.v1.EventWorkflowError".to_owned(),
        value: proto::EventWorkflowError {
            code: detail_code as i32,
            current: None,
        }
        .encode_to_vec(),
    })
}

pub(super) fn query_filter(
    principal: &ApiPrincipal,
    request: &proto::EventWorkflowQuery,
) -> Result<workflow::Query, ControlCommandError> {
    let actor = actor_id(principal, &request.local_workspace_id)?;
    let review = match proto::EventReviewFilter::try_from(request.review) {
        Ok(proto::EventReviewFilter::Any) => workflow::ReviewFilter::Any,
        Ok(proto::EventReviewFilter::Unreviewed) => workflow::ReviewFilter::Unreviewed,
        Ok(proto::EventReviewFilter::Reviewed) => workflow::ReviewFilter::Reviewed,
        Ok(proto::EventReviewFilter::Dismissed) => workflow::ReviewFilter::Dismissed,
        Err(_) => return Err(invalid("event review filter is invalid")),
    };
    Ok(workflow::Query {
        actor_id: actor,
        review,
        bookmarked: request.bookmarked,
        bookmarked_by_me: request.bookmarked_by_me,
    })
}

pub(super) const fn proto_counts(counts: workflow::Counts) -> proto::EventWorkflowCounts {
    proto::EventWorkflowCounts {
        total: counts.total,
        unreviewed: counts.unreviewed,
        reviewed: counts.reviewed,
        dismissed: counts.dismissed,
        bookmarked: counts.bookmarked,
        bookmarked_by_me: counts.bookmarked_by_me,
    }
}

pub(super) fn actor_id(
    principal: &ApiPrincipal,
    workspace: &str,
) -> Result<String, ControlCommandError> {
    if !principal.is_local() {
        if !workspace.is_empty() {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "authenticated reviewers must not supply a local workspace identity",
            ));
        }
        return Ok(principal.id());
    }
    let workspace = uuid::Uuid::parse_str(workspace).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "local event review requires a persistent workspace UUID",
        )
    })?;
    if workspace.is_nil() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "local workspace UUID must not be nil",
        ));
    }
    Ok(format!("workspace:{workspace}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::ApiPrincipalIdentity;

    #[test]
    fn event_workflow_control_response_rejects_oversized_metadata() {
        let result = proto::EventWorkflowResult {
            states: vec![proto::EventWorkflowState {
                bookmark: Some(proto::EventBookmark {
                    note: "x".repeat(65_536),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(bounded_result(result).is_err());
    }

    #[test]
    fn event_workflow_storage_failure_is_unavailable_and_does_not_expose_database_details() {
        let error = command_error(anyhow::anyhow!(
            "database failed at /private/operator/recordings.db"
        ));
        assert_eq!(error.code, proto::ErrorCode::Unavailable);
        assert!(!error.message.contains("/private"));
        let detail = proto::EventWorkflowError::decode(error.details[0].value.as_slice()).unwrap();
        assert_eq!(
            detail.code,
            proto::EventWorkflowErrorCode::Unavailable as i32
        );
    }

    #[test]
    fn workflow_actor_uses_stable_local_workspace_and_never_a_remote_override() {
        let local = ApiPrincipal::local("127.0.0.1".parse().unwrap());
        let workspace = "00000000-0000-4000-8000-000000000121";
        assert_eq!(
            actor_id(&local, workspace).unwrap(),
            format!("workspace:{workspace}")
        );
        assert!(actor_id(&local, "").is_err());
        assert!(actor_id(&local, "principal-other").is_err());
        let credential = ApiPrincipal {
            identity: ApiPrincipalIdentity::Credential {
                id: uuid::Uuid::from_u128(121),
                revision: 7,
            },
            display_name: "Reviewer".to_owned(),
            role: crate::access::AccessRole::User,
            credential_expires_at_ms: None,
        };
        assert_eq!(actor_id(&credential, "").unwrap(), credential.id());
        assert!(actor_id(&credential, workspace).is_err());
    }
}
