use super::*;
use crate::shares::CreateGrant;
use proto::{share_command::Action, share_result::Result as Value};

#[cfg(test)]
pub(in crate::server) fn dispatch(
    state: &ServerState,
    principal: &ApiPrincipal,
    command: proto::ShareCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    dispatch_for_session(state, SessionId::from_u64(0), principal, command)
}

pub(in crate::server) fn dispatch_for_session(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    command: proto::ShareCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    if principal.role != AccessRole::Administrator {
        return Err(denied());
    }
    let value = match command.action {
        Some(Action::GetPolicy(_)) => Value::Policy(wire::policy(state)?),
        Some(Action::ConfigurePolicy(request)) => Value::Policy(configure(state, request)?),
        Some(Action::PreviewExport(request)) => {
            Value::Evidence(wire::evidence(&evidence::preview(
                state,
                principal,
                &request.export_job_id,
                request.recording_range.as_ref(),
            )?))
        }
        Some(Action::Create(request)) => {
            Value::Created(create(state, session_id, principal, request)?)
        }
        Some(Action::List(request)) => Value::Grants(list(state, principal, request)?),
        Some(Action::Revoke(request)) => Value::Grant(revoke(state, request)?),
        Some(Action::RevokeEvidence(request)) => {
            Value::RevokedEvidence(revoke_evidence(state, request)?)
        }
        None => return Err(invalid("sharing action is required")),
    };
    Ok(control_ok::Result::ShareResult(proto::ShareResult {
        result: Some(value),
    }))
}

fn configure(
    state: &ServerState,
    request: proto::ConfigureSharePolicy,
) -> Result<proto::SharePolicy, ControlCommandError> {
    configure_with(state, request, config::sharing::update)
}

pub(super) fn configure_with(
    state: &ServerState,
    request: proto::ConfigureSharePolicy,
    persist: impl FnOnce(
        &Path,
        u64,
        config::sharing::SharingConfig,
    ) -> Result<Config, config::sharing::UpdateError>,
) -> Result<proto::SharePolicy, ControlCommandError> {
    let _configuration = coordination(state)?;
    let settings = request
        .settings
        .ok_or_else(|| invalid("sharing settings are required"))?;
    let revision = request
        .expected_revision
        .parse::<u64>()
        .map_err(|_| invalid("invalid sharing revision"))?;
    let path = state
        .camera_config_path
        .as_deref()
        .ok_or_else(unavailable)?;
    let current = config::load_config(path).map_err(|_| unavailable())?;
    if revision != current.sharing.revision {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "sharing revision conflicts",
        ));
    }
    let candidate = config::sharing::SharingConfig {
        enabled: settings.enabled,
        public_origin: settings.public_origin,
        revision,
    };
    candidate
        .validate()
        .map_err(|_| invalid("sharing requires a canonical HTTPS origin"))?;
    match persist(path, revision, candidate) {
        Ok(updated) => activate(state, &updated).map_err(|_| unavailable())?,
        Err(config::sharing::UpdateError::Validation) => return Err(unavailable()),
        Err(config::sharing::UpdateError::Persistence) => {
            persistence_failed(state, path).map_err(|_| unavailable())?;
            return Err(ControlCommandError::new(
                proto::ErrorCode::Unavailable,
                503,
                "sharing policy write could not be confirmed; active guest access has stopped",
            ));
        }
    }
    wire::policy(state)
}

fn create(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    request: proto::CreateShareGrant,
) -> Result<proto::IssuedShareGrant, ControlCommandError> {
    let (inspected, _lease) = evidence::inspect(
        state,
        principal,
        &request.export_job_id,
        request.recording_range.as_ref(),
    )?;
    create_inspected(state, session_id, principal, request, inspected)
}

pub(super) fn create_inspected(
    state: &ServerState,
    session_id: SessionId,
    principal: &ApiPrincipal,
    request: proto::CreateShareGrant,
    inspected: evidence::ValidatedEvidence,
) -> Result<proto::IssuedShareGrant, ControlCommandError> {
    let _configuration = coordination(state)?;
    let context = context(state)?;
    let issuer = principal.sharing_issuer();
    if !authentication::sharing_administrator(
        state,
        &issuer,
        i64::try_from(unix_time_ms()).map_err(|_| unavailable())?,
    ) {
        return Err(denied());
    }
    let evidence = evidence::preview_binding(
        state,
        principal,
        &request.export_job_id,
        request.recording_range.as_ref(),
    )?;
    if evidence.binding != inspected.binding
        || evidence.binding.artifact_id != request.artifact_id
        || encode_lower_hex(evidence.binding.sha256) != request.expected_sha256
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "export preview changed",
        ));
    }
    let exact_range = request.recording_range.is_some();
    let intent = CreateGrant {
        request_id: identity(&request.request_id)?,
        issuer,
        binding: evidence.binding,
        label: request.display_name,
        note: request.note,
        expires_at_ms: request.expires_at_ms,
        allow_stream: request.allow_stream,
        allow_download: request.allow_download,
        max_views: request.max_views,
        max_downloads: request.max_downloads,
    };
    let (issued, now_ms) = session_lifecycle::admit(state, session_id, || {
        with_store(state, |store, clock| {
            Ok((
                store.create_with_evidence(intent, clock, exact_range)?,
                clock.unix_ms,
            ))
        })
    })?;
    let url = format!("{}/share#{}", context.origin, issued.token.into_string());
    Ok(proto::IssuedShareGrant {
        grant: Some(wire::grant(state, issued.grant, now_ms)),
        url,
    })
}

fn list(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::ListShareGrants,
) -> Result<proto::ShareGrantList, ControlCommandError> {
    let size = if request.page_size == 0 {
        50
    } else {
        request.page_size
    };
    if size > 100 {
        return Err(invalid("sharing page size must be at most 100"));
    }
    let after = request
        .after_grant_id
        .as_deref()
        .map(identity)
        .transpose()?;
    let issuer = principal.sharing_issuer();
    let (mut records, now_ms) = with_store(state, |store, clock| {
        let records = if request.all_creators {
            store.all(clock)?
        } else {
            store.list(&issuer, clock)?
        };
        Ok((records, clock.unix_ms))
    })?;
    records.retain(|grant| {
        let ended_at_ms = grant
            .revoked_at_ms
            .map_or(grant.intent.expires_at_ms, |revoked| {
                revoked.min(grant.intent.expires_at_ms)
            });
        if now_ms.saturating_sub(ended_at_ms) >= crate::shares::AUDIT_RETENTION_MS {
            return false;
        }
        after.is_none_or(|after| grant.id > after)
    });
    records.sort_unstable_by_key(|grant| grant.id);
    page(state, records, now_ms, size)
}

fn page(
    state: &ServerState,
    records: Vec<GrantSummary>,
    now_ms: i64,
    size: u32,
) -> Result<proto::ShareGrantList, ControlCommandError> {
    let mut result = proto::ShareGrantList::default();
    let mut bytes = 64;
    let mut more = false;
    for record in records {
        if result.grants.len() >= usize::try_from(size).map_err(|_| unavailable())? {
            more = true;
            break;
        }
        let grant = wire::grant(state, record, now_ms);
        // Reserve envelope overhead and cap each response below the existing control-message budget.
        let next = bytes + grant.encoded_len() + 10;
        if next > 48 * 1024 {
            more = true;
            break;
        }
        bytes = next;
        result.grants.push(grant);
    }
    if more && result.grants.is_empty() {
        return Err(unavailable());
    }
    result.next_after_grant_id = more.then(|| {
        result
            .grants
            .last()
            .expect("one bounded grant fits a page")
            .grant_id
            .clone()
    });
    Ok(result)
}

fn revoke(
    state: &ServerState,
    request: proto::RevokeShareGrant,
) -> Result<proto::ShareGrant, ControlCommandError> {
    let id = identity(&request.grant_id)?;
    let (grant, now_ms) = with_store(state, |store, clock| {
        Ok((
            store.revoke(id, request.expected_revision, &request.reason, clock)?,
            clock.unix_ms,
        ))
    })?;
    state.guest_review.abort_grants(&[id]);
    Ok(wire::grant(state, grant, now_ms))
}

fn revoke_evidence(
    state: &ServerState,
    request: proto::RevokeShareEvidence,
) -> Result<proto::RevokedShareEvidence, ControlCommandError> {
    let selector = request
        .evidence
        .ok_or_else(|| invalid("evidence selector is required"))?;
    let source_identity = match &selector {
        proto::revoke_share_evidence::Evidence::SourceIdentity(id) => Some(identity(id)?),
        proto::revoke_share_evidence::Evidence::ArtifactId(_) => None,
    };
    if let proto::revoke_share_evidence::Evidence::ArtifactId(id) = &selector
        && !safe_export_job_id(id)
    {
        return Err(invalid("invalid artifact identity"));
    }
    let revoked = with_store(state, |store, clock| {
        let selected = store
            .all(clock)?
            .into_iter()
            .filter(|grant| {
                grant.revoked_at_ms.is_none() && clock.unix_ms < grant.intent.expires_at_ms
            })
            .filter(|grant| match &selector {
                proto::revoke_share_evidence::Evidence::ArtifactId(id) => {
                    grant.intent.binding.artifact_id == *id
                }
                proto::revoke_share_evidence::Evidence::SourceIdentity(_) => {
                    Some(grant.intent.binding.source_identity) == source_identity
                }
            })
            .map(|grant| (grant.id, grant.revision))
            .collect::<Vec<_>>();
        store.revoke_batch(&selected, &request.reason, clock)
    })?;
    let ids = revoked.iter().map(|grant| grant.id).collect::<Vec<_>>();
    state.guest_review.abort_grants(&ids);
    Ok(proto::RevokedShareEvidence {
        revoked_count: u32::try_from(ids.len()).map_err(|_| unavailable())?,
    })
}

fn identity(value: &str) -> Result<Uuid, ControlCommandError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or_else(|| invalid("invalid sharing identity"))
}

fn coordination(state: &ServerState) -> Result<std::sync::MutexGuard<'_, ()>, ControlCommandError> {
    state.config_update.try_lock().map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "configuration coordination is busy",
        )
    })
}
