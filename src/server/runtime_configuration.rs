use super::{
    ControlCommandError, RuntimeSettingsUpdate, RuntimeSettingsUpdateResponse,
    RuntimeStorageSettingsUpdate, ServerState, current_config, normalize_storage_path,
    proto_runtime_configuration_result, save_runtime_settings, storage_write_probe,
};
use crate::{
    api::proto::{self, ok as control_ok, runtime_configuration_command},
    config,
};
use std::path::Path;

pub(super) fn dispatch(
    state: &ServerState,
    command: proto::RuntimeConfigurationCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    match command.action {
        Some(runtime_configuration_command::Action::Get(_)) => {
            Ok(control_ok::Result::RuntimeConfigurationResult(
                proto_runtime_configuration_result(RuntimeSettingsUpdateResponse {
                    config: current_config(state),
                    restart_required: false,
                }),
            ))
        }
        Some(runtime_configuration_command::Action::Update(update)) => {
            let port = u16::try_from(update.port).map_err(|_| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "server port must be between 1 and 65535",
                )
            })?;
            let Some(storage) = update.storage else {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "runtime configuration requires storage settings",
                ));
            };
            let current_storage = current_config(state).storage;
            let write_buffer_bytes = usize::try_from(storage.write_buffer_bytes).map_err(|_| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "write buffer size is too large",
                )
            })?;
            let maximum_used_percent = storage.maximum_used_percent.map_or_else(
                || Ok(current_storage.maximum_used_percent),
                |percent| {
                    if percent == 0 {
                        return Ok(None);
                    }
                    u8::try_from(percent)
                        .map_err(|_| {
                            ControlCommandError::new(
                                proto::ErrorCode::InvalidRequest,
                                400,
                                "maximum filesystem usage percentage is too large",
                            )
                        })
                        .map(Some)
                },
            )?;
            let result = save_runtime_settings(
                RuntimeSettingsUpdate {
                    host: update.host,
                    port,
                    expected_configuration_revision: update.expected_configuration_revision,
                    storage: RuntimeStorageSettingsUpdate {
                        medium_term_path: storage.medium_term_path,
                        long_term_path: storage.long_term_path,
                        recording_catalog_path: storage.recording_catalog_path,
                        event_thumbnail_path: storage.event_thumbnail_path,
                        event_thumbnail_max_mb: storage.event_thumbnail_max_mb,
                        short_term_secs: storage.short_term_secs,
                        medium_term_secs: storage.medium_term_secs,
                        flush_interval_secs: storage.flush_interval_secs,
                        write_buffer_bytes,
                        long_term_max_gb: storage.long_term_max_gb,
                        minimum_free_gb: storage
                            .minimum_free_gb
                            .unwrap_or(current_storage.minimum_free_gb),
                        maximum_used_percent,
                        warning_free_gb: storage
                            .warning_free_gb
                            .unwrap_or(current_storage.warning_free_gb),
                        critical_free_gb: storage
                            .critical_free_gb
                            .unwrap_or(current_storage.critical_free_gb),
                        cleanup_hysteresis_gb: storage
                            .cleanup_hysteresis_gb
                            .unwrap_or(current_storage.cleanup_hysteresis_gb),
                    },
                    move_existing_recordings: update.move_existing_recordings,
                },
                state,
            )?;
            Ok(control_ok::Result::RuntimeConfigurationResult(
                proto_runtime_configuration_result(result),
            ))
        }
        Some(runtime_configuration_command::Action::ProbeStorage(request)) => {
            let path = if let Some(config_path) = &state.camera_config_path {
                config::resolve_secret_references(config_path, &request.path).map_err(|error| {
                    ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        format!("storage path secret reference is invalid: {error}"),
                    )
                })?
            } else {
                request.path
            };
            let path = normalize_storage_path(&path).ok_or_else(|| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "storage path must be nonempty and cannot contain NUL",
                )
            })?;
            let result = storage_write_probe(Path::new(&path));
            Ok(control_ok::Result::StorageWriteProbeResult(
                proto::StorageWriteProbeResult {
                    writable: result.is_ok(),
                    detail: match result {
                        Ok(()) => "Write, flush, rename, and cleanup succeeded.".to_owned(),
                        Err(error) => format!("Storage write verification failed: {error}"),
                    },
                },
            ))
        }
        None => Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "runtime configuration command has no action",
        )),
    }
}
