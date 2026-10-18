use super::*;
use crate::keeppeek::CameraProfileIds;

pub(super) fn camera_profile_configuration(
    state: &ServerState,
) -> (Option<Config>, Option<CameraProfileIds>) {
    let configuration = state
        .camera_config_path
        .as_deref()
        .and_then(|path| config::load_config(path).ok());
    let identities = configuration
        .as_ref()
        .and_then(|configuration| configuration.camera_profile_identities().ok())
        .and_then(|identities| CameraProfileIds::new(identities).ok());
    (configuration, identities)
}

pub(super) fn camera_profile_ids(state: &ServerState) -> anyhow::Result<CameraProfileIds> {
    let identities = state
        .camera_config_path
        .as_deref()
        .map(|path| config::load_config(path)?.camera_profile_identities())
        .transpose()?
        .unwrap_or_default();
    CameraProfileIds::new(identities)
}

pub(super) fn configured_profile_ids(identities: &CameraProfileIds) -> BTreeSet<String> {
    identities
        .entries()
        .map(|(identity, _)| identity.clone())
        .collect()
}

pub(super) fn cameras_entry(state: &ServerState) -> Result<StoredEntry, Error> {
    let identities =
        camera_profile_ids(state).map_err(|error| Error::Storage(error.to_string()))?;
    let cameras = identities
        .entries()
        .map(|(identity, ip)| (identity.clone(), ip.to_string()))
        .collect::<BTreeMap<_, _>>();
    let value =
        super::super::super::peek_layouts::json_to_struct(serde_json::json!({"cameras": cameras}))
            .map_err(|error| Error::Storage(error.to_string()))?;
    if value.encoded_len() > PROFILE_DOCUMENT_BYTES_MAX {
        return Err(Error::Invalid(Invalid::ValueTooLarge));
    }
    Ok(StoredEntry {
        namespace: NAMESPACE.to_owned(),
        key: CAMERAS_KEY.to_owned(),
        schema: CAMERAS_SCHEMA.to_owned(),
        value,
        revision: state
            .runtime_profiles
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revision,
        updated_ms: 0,
        expires_ms: None,
        owner_id: "server".to_owned(),
    })
}
