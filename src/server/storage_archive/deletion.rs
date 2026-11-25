//! Maps Administrator-only archive deletion requests to the durable catalog transaction.
use super::*;
use crate::storage::catalog::{
    archive::deletion::{Plan, Policy, Request, Selection},
    maintenance::jobs::Nonce,
};

fn admission_policy(state: &ServerState, handle: &Handle, confirming: bool) -> Result<Policy> {
    let policy = handle.deletion_policy().map_err(operation_failed)?;
    if let Some(path) = &state.camera_config_path {
        let config = crate::config::load_config(path).map_err(operation_failed)?;
        let saved = config.storage.archive.as_ref().ok_or_else(unavailable)?;
        if confirming {
            crate::backup::check_staged_archive_deletion(path, saved).map_err(|_| {
                ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    409,
                    "pending restore prevents archive deletion confirmation",
                )
            })?;
        }
        if Policy::from_settings(saved).map_err(operation_failed)? != policy {
            return Err(unavailable());
        }
    }
    Ok(policy)
}

pub(super) fn preview(
    state: &ServerState,
    principal: &ApiPrincipal,
    handle: &Handle,
    input: proto::PreviewArchiveDeletion,
) -> Result<proto::ArchiveDeletionPreview> {
    use proto::preview_archive_deletion::Selection as Wire;
    let selection = match input.selection {
        Some(Wire::JobId(id)) => {
            require_job_id(&id)?;
            Selection::Explicit(id)
        }
        Some(Wire::RetentionPage(page)) => {
            if !page.after_job_id.is_empty() {
                require_job_id(&page.after_job_id)?;
            }
            if page.limit > 16 {
                return Err(invalid("archive deletion preview supports up to 16 jobs"));
            }
            Selection::Retention {
                after: page.after_job_id,
                limit: u16::try_from(if page.limit == 0 { 16 } else { page.limit })
                    .expect("bounded archive deletion page"),
            }
        }
        None => return Err(invalid("archive deletion selection is required")),
    };
    let _configuration = state.config_update.lock().map_err(|_| unavailable())?;
    let policy = admission_policy(state, handle, false)?;
    let plan = handle
        .deletion(Request::Preview {
            actor: principal.id(),
            policy,
            selection,
        })
        .map_err(operation_failed)?;
    wire(state, plan)
}

pub(super) fn confirm(
    state: &ServerState,
    principal: &ApiPrincipal,
    handle: Option<&Handle>,
    input: proto::ConfirmArchiveDeletion,
) -> Result<proto::ArchiveDeletionPreview> {
    require_job_id(&input.plan_id)?;
    if input.expected_revision.len() != 64 {
        return Err(invalid("archive deletion revision is invalid"));
    }
    let nonce = Nonce::parse(&input.confirmation_nonce)
        .map_err(|_| invalid("archive deletion confirmation is invalid"))?;
    let _configuration = state.config_update.lock().map_err(|_| unavailable())?;
    if let Some(catalog) = &state.catalog {
        use crate::storage::catalog::archive;
        let replay = Request::ReplayConfirmation {
            actor: principal.id(),
            plan: input.plan_id.clone(),
            revision: input.expected_revision.clone(),
            nonce: nonce.clone(),
        };
        match catalog
            .archive(archive::Request::Deletion(replay))
            .map_err(operation_failed)?
        {
            archive::Reply::Deletion(plan) => return wire(state, *plan),
            archive::Reply::Done => {}
            _ => return Err(unavailable()),
        }
    }
    let handle = handle.ok_or_else(unavailable)?;
    let policy = admission_policy(state, handle, true)?;
    let plan = handle
        .deletion(Request::Confirm {
            actor: principal.id(),
            policy,
            plan: input.plan_id,
            revision: input.expected_revision,
            nonce,
        })
        .map_err(operation_failed)?;
    wire(state, plan)
}

fn wire(state: &ServerState, plan: Plan) -> Result<proto::ArchiveDeletionPreview> {
    let target_id = target_names(state)
        .unwrap_or_default()
        .filter(|(configured, _)| configured == &plan.target)
        .map_or_else(|| "unavailable-target".into(), |(_, public)| public);
    let selected = plan.items.iter().filter(|i| {
        matches!(
            i.state.as_str(),
            "eligible" | "queued" | "claimed" | "retired" | "acknowledged" | "blocked"
        )
    });
    let (selected_count, selected_bytes) = selected.fold((0_u32, 0_u64), |(count, bytes), item| {
        (count + 1, bytes + item.bytes)
    });
    Ok(proto::ArchiveDeletionPreview {
        plan_id: plan.id,
        revision: plan.revision,
        confirmation_nonce: plan.nonce.map_or_else(String::new, |n| n.as_str().into()),
        expires_at_ms: plan.expires_ms,
        target_id,
        items: plan
            .items
            .into_iter()
            .map(|item| proto::ArchiveDeletionItem {
                job_id: item.job_id,
                object_id: item.object_id,
                kind: if item.kind == "recording" {
                    proto::StorageObjectKind::Recording as i32
                } else {
                    proto::StorageObjectKind::Export as i32
                },
                bytes: item.bytes,
                verified_at_ms: item.verified_ms,
                state: item.state,
            })
            .collect(),
        next_after_job_id: plan.next,
        selected_count,
        selected_bytes,
        confirmed: plan.confirmed,
    })
}
