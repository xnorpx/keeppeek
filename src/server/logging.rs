use super::{
    ControlCommandError, ServerState, get_logging_settings, proto_logging_settings,
    set_logging_filter,
};
use crate::api::proto::{self, logging_command, ok as control_ok};

pub(super) fn dispatch(
    state: &ServerState,
    command: proto::LoggingCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    let settings = match command.action {
        Some(logging_command::Action::GetSettings(_)) => get_logging_settings(state)?,
        Some(logging_command::Action::SetFilter(update)) => {
            set_logging_filter(state, &update.filter)?
        }
        None => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "logging command has no action",
            ));
        }
    };
    Ok(control_ok::Result::LoggingSettingsResult(
        proto_logging_settings(settings),
    ))
}
