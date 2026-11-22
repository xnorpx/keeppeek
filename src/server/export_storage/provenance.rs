use super::*;

pub(in crate::server) fn create_for_principal(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::CreateExportJob,
) -> Result<proto::ExportJob, ControlCommandError> {
    super::creation::create_with_owner(
        state,
        &principal.id(),
        Some(principal.sharing_issuer().creator_id()),
        request,
        super::creation::TimingSource::Configured,
    )
}

pub(in crate::server) fn retry_for_principal(
    state: &ServerState,
    principal: &ApiPrincipal,
    id: &str,
) -> Result<proto::ExportJob, ControlCommandError> {
    super::creation::retry_with_owner(
        state,
        &principal.id(),
        Some(principal.sharing_issuer().creator_id()),
        id,
    )
}

pub(super) fn source_identity(state: &ServerState, source: &str) -> Option<Uuid> {
    let path = state.camera_config_path.as_deref()?;
    let address = source.parse::<IpAddr>().ok()?;
    let configuration = config::load_config(path).ok()?;
    let identities = configuration.camera_profile_identities().ok()?;
    Uuid::parse_str(identities.get(&address)?)
        .ok()
        .filter(|identity| !identity.is_nil())
}
