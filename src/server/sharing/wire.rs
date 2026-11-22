use super::*;

pub(super) fn evidence(value: &ValidatedEvidence) -> proto::ShareEvidence {
    let binding = &value.binding;
    proto::ShareEvidence {
        export_job_id: binding.job_id.clone(),
        artifact_id: binding.artifact_id.clone(),
        sha256: encode_lower_hex(binding.sha256),
        source_id: binding.source_id.clone(),
        source_identity: binding.source_identity.to_string(),
        camera_name: value.camera_name.clone(),
        start_ms: binding.start_ms,
        end_ms: binding.end_ms,
        bytes: binding.bytes,
        available_until_ms: binding.available_until_ms,
        has_gaps: value.has_gaps,
    }
}

pub(super) fn policy(state: &ServerState) -> Result<proto::SharePolicy, ControlCommandError> {
    let settings = settings(state)?;
    let available = settings.enabled
        && settings.public_origin.is_some()
        && state.sharing.page_csp.is_some()
        && state.camera_config_path.is_some()
        && state.catalog.is_some()
        && state.storage_config.volume_runtime.is_some()
        && state
            .sharing
            .store
            .lock()
            .map_err(|_| unavailable())?
            .is_some();
    Ok(proto::SharePolicy {
        revision: settings.revision.to_string(),
        settings: Some(proto::SharePolicySettings {
            enabled: settings.enabled,
            public_origin: settings.public_origin,
        }),
        available,
        unavailable_reason: state
            .sharing
            .policy
            .read()
            .map_err(|_| unavailable())?
            .error
            .map(str::to_owned)
            .or_else(|| {
                (!available).then(|| {
                    "Configure HTTPS sharing and an available private metadata owner.".into()
                })
            }),
    })
}

pub(super) fn grant(state: &ServerState, value: GrantSummary, now_ms: i64) -> proto::ShareGrant {
    let current = validate_guest(state, &value)
        .ok()
        .and_then(|evidence| open_guest(state, &evidence.binding).ok().map(|_| evidence));
    let status = status(&value, now_ms, current.is_some());
    let fallback = ValidatedEvidence {
        binding: value.intent.binding.clone(),
        camera_name: "Unavailable camera".into(),
        has_gaps: true,
    };
    let evidence = evidence(current.as_ref().unwrap_or(&fallback));
    proto::ShareGrant {
        grant_id: value.id.to_string(),
        revision: value.revision,
        creator_id: value.intent.issuer.creator_id(),
        display_name: value.intent.label,
        note: value.intent.note,
        created_at_ms: value.created_at_ms,
        expires_at_ms: value.intent.expires_at_ms,
        revoked_at_ms: value.revoked_at_ms,
        revoke_reason: value.revoke_reason,
        status: status as i32,
        allow_stream: value.intent.allow_stream,
        allow_download: value.intent.allow_download,
        max_views: value.intent.max_views,
        max_downloads: value.intent.max_downloads,
        views: value.views,
        downloads: value.downloads,
        last_access_at_ms: value.last_access_at_ms,
        evidence: Some(evidence),
    }
}

fn status(value: &GrantSummary, now_ms: i64, available: bool) -> proto::ShareGrantStatus {
    if value.revoked_at_ms.is_some() {
        return proto::ShareGrantStatus::Revoked;
    }
    if now_ms >= value.intent.expires_at_ms {
        return proto::ShareGrantStatus::Expired;
    }
    if !available {
        return proto::ShareGrantStatus::Unavailable;
    }
    let stream_exhausted =
        !value.intent.allow_stream || value.views >= value.intent.max_views.unwrap_or(u32::MAX);
    let download_exhausted = !value.intent.allow_download
        || value.downloads >= value.intent.max_downloads.unwrap_or(u32::MAX);
    if stream_exhausted && download_exhausted {
        proto::ShareGrantStatus::LimitReached
    } else {
        proto::ShareGrantStatus::Active
    }
}
