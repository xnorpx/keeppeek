use super::{
    ApiControlRuntime, ApiSessionControl, Arc, ControlDispatch, ControlRequestHandler, ErrorCode,
    Ordering, background_operation_slot, control_response, enqueue_control_dispatch,
    envelope_dispatch, failed_control_dispatch, spawn_background_control_operation,
};
use crate::api::proto::{Request, request::Command, stored_media_command::Action};

type Deferred = (Request, Arc<dyn ControlRequestHandler>);

#[derive(Default)]
pub(super) struct Selection {
    cursor: Option<String>,
    deferred: Option<Deferred>,
}

impl Selection {
    pub(super) fn finish(&mut self, dispatch: &mut ControlDispatch) -> Option<Deferred> {
        self.cursor = None;
        let deferred = self.deferred.take()?;
        if matches!(
            dispatch.response.result.as_ref(),
            Some(control_response::Result::Ok(_))
        ) {
            // Do not launch the selected picture transfer after the viewer resumed or sought.
            // Preserve actual authorization failures and their session close callbacks.
            *dispatch = failed_control_dispatch(
                dispatch.response.request_id,
                ErrorCode::Rejected,
                "frame selection superseded by playback",
            );
        }
        Some(deferred)
    }
}

fn stored_action(request: &Request) -> Option<&Action> {
    match request.command.as_ref()? {
        Command::StoredMediaCommand(command) => command.action.as_ref(),
        _ => None,
    }
}

pub(super) fn dispatch(
    request: Request,
    handler: Arc<dyn ControlRequestHandler>,
    control: &ApiSessionControl,
    runtime: &mut ApiControlRuntime,
) -> anyhow::Result<()> {
    let request_id = request.request_id;
    let operation = background_operation_slot(control, &request);
    let error = if operation.swap(true, Ordering::AcqRel) {
        let cursor = match stored_action(&request) {
            Some(Action::Seek(seek)) => Some(&seek.stored_media_id),
            Some(Action::SetPlayback(update)) if update.playing == Some(true) => {
                Some(&update.stored_media_id)
            }
            _ => None,
        };
        if cursor.is_some()
            && cursor == runtime.selection.cursor.as_ref()
            && runtime.selection.deferred.is_none()
        {
            // One follow-up waits for the bounded selector. Never create parallel disk
            // reads on this peer or cancel ordinary playback chunks while it waits.
            runtime.selection.deferred = Some((request, handler));
            return Ok(());
        }
        failed_control_dispatch(
            request_id,
            ErrorCode::Rejected,
            "another background control operation is already in progress",
        )
    } else {
        let selected_cursor = match stored_action(&request) {
            Some(Action::StepFrame(step)) => Some(step.stored_media_id.clone()),
            _ => None,
        };
        let Err(error) = spawn_background_control_operation(
            request,
            handler,
            control,
            runtime.dispatch_tx.clone(),
        ) else {
            if let Some(cursor) = selected_cursor {
                runtime.selection.cursor = Some(cursor);
            }
            return Ok(());
        };
        operation.store(false, Ordering::Release);
        failed_control_dispatch(
            request_id,
            ErrorCode::Unavailable,
            format!("unable to start background control operation: {error}"),
        )
    };
    enqueue_control_dispatch(envelope_dispatch(error), runtime)
}

#[cfg(test)]
mod tests;
