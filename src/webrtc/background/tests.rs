use super::{Action, Command, Request, dispatch};
use crate::api::proto::{self, StoredMediaCommand};
use crate::webrtc::{
    ApiControlRuntime, ApiMediaRuntime, ControlDispatch, ControlEnvelope, ControlRequestHandler,
    DataChannelTarget, OutboundDataMessage, QueuedApiData, QueuedControlOutput, control_envelope,
    control_response, drain_pending_control_dispatches, tests::test_background_control,
};
use crossbeam_channel::{Receiver, Sender, bounded};
use prost::Message;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

struct Handler {
    started: Sender<u64>,
    release: Receiver<()>,
}

fn blocked_handler() -> (Arc<Handler>, Receiver<u64>, Sender<()>) {
    let (started, received) = bounded(1);
    let (release, proceed) = bounded(0);
    (
        Arc::new(Handler {
            started,
            release: proceed,
        }),
        received,
        release,
    )
}

impl ControlRequestHandler for Handler {
    fn handle(&self, request: Request) -> ControlDispatch {
        self.started.send(request.request_id).unwrap();
        self.release.recv_timeout(Duration::from_secs(5)).unwrap();
        ControlDispatch {
            response: proto::Response {
                request_id: request.request_id,
                result: Some(control_response::Result::Ok(proto::Ok::default())),
            },
            after_send: (request.request_id == 1).then(|| -> crate::webrtc::PostSendAction {
                Box::new(|| panic!("superseded frame must never launch a transfer"))
            }),
            data_messages: Vec::new(),
            notifications: Vec::new(),
        }
    }
}

fn request(id: u64, action: Action) -> Request {
    Request {
        request_id: id,
        command: Some(Command::StoredMediaCommand(StoredMediaCommand {
            action: Some(action),
        })),
    }
}

fn complete_worker(runtime: &ApiControlRuntime) {
    let done = runtime
        .dispatch_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(runtime.dispatch_tx.try_send(done).is_ok());
}

fn check_successor(action: Action, expected_groups: &[&str]) {
    let control = test_background_control();
    control
        .background_operation_in_flight
        .store(false, Ordering::Release);
    let (dispatch_tx, dispatch_rx) = bounded(1);
    let mut runtime = ApiControlRuntime {
        selection: Default::default(),
        dispatch_tx,
        dispatch_rx,
        control_output_ready: true,
        outbound: Default::default(),
        outbound_bytes: 0,
        outbound_messages: 0,
        after_send: Vec::new(),
    };
    let mut media = ApiMediaRuntime::default();
    for group in [
        "stored:cursor",
        "frame-step:cursor",
        "stored:another",
        "frame-step:another",
    ] {
        media
            .enqueue(vec![OutboundDataMessage {
                permit: None,
                target: DataChannelTarget::Reliable,
                group: group.into(),
                message: proto::Message::default(),
            }])
            .unwrap();
    }
    let (handler, received, release) = blocked_handler();
    let step = Action::StepFrame(proto::StepStoredFrame {
        stored_media_id: "cursor".into(),
        ..Default::default()
    });
    dispatch(request(1, step), handler.clone(), &control, &mut runtime).unwrap();
    assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    let (share, share_started, release_share) = blocked_handler();
    let preview = Request {
        request_id: 4,
        command: Some(Command::ShareCommand(proto::ShareCommand {
            action: Some(proto::share_command::Action::PreviewExport(
                proto::PreviewShareExport {
                    recording_range: Some(proto::ShareRecordingRange::default()),
                    ..Default::default()
                },
            )),
        })),
    };
    dispatch(preview, share, &control, &mut runtime).unwrap();
    assert_eq!(
        share_started.recv_timeout(Duration::from_secs(5)).unwrap(),
        4
    );
    let other = Action::Seek(proto::SeekStoredMedia {
        stored_media_id: "another".into(),
        ..Default::default()
    });
    dispatch(request(3, other), handler.clone(), &control, &mut runtime).unwrap();
    dispatch(request(2, action), handler, &control, &mut runtime).unwrap();
    release_share.send(()).unwrap();
    complete_worker(&runtime);
    drain_pending_control_dispatches(&control, &mut runtime, &mut media).unwrap();
    assert!(!control.share_inspection_in_flight.load(Ordering::Acquire));
    assert!(
        control
            .background_operation_in_flight
            .load(Ordering::Acquire)
    );
    assert_eq!(runtime.selection.cursor.as_deref(), Some("cursor"));
    assert!(runtime.selection.deferred.is_some());
    assert!(received.is_empty());
    assert_eq!(media.outbound.len(), 4);
    release.send(()).unwrap();
    complete_worker(&runtime);
    drain_pending_control_dispatches(&control, &mut runtime, &mut media).unwrap();
    assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
    assert_eq!(media.outbound.len(), 4);
    release.send(()).unwrap();
    complete_worker(&runtime);
    drain_pending_control_dispatches(&control, &mut runtime, &mut media).unwrap();
    assert!(
        !control
            .background_operation_in_flight
            .load(Ordering::Acquire)
    );
    let remaining: Vec<_> = media.outbound.iter().map(QueuedApiData::group).collect();
    assert_eq!(remaining, expected_groups);
    let replies = runtime
        .outbound
        .into_iter()
        .map(|output| {
            let QueuedControlOutput::Payload(payload) = output else {
                panic!("stale frame side effect")
            };
            let Some(control_envelope::Message::Response(response)) =
                ControlEnvelope::decode(payload.as_slice()).unwrap().message
            else {
                panic!("unexpected envelope")
            };
            (
                response.request_id,
                matches!(response.result, Some(control_response::Result::Ok(_))),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(replies, [(3, false), (4, true), (1, false), (2, true)]);
}

#[test]
fn resume_and_cold_seek_before_frame_ready_wait_for_the_same_cursor_and_preserve_media() {
    check_successor(
        Action::SetPlayback(proto::SetStoredMediaPlayback {
            stored_media_id: "cursor".into(),
            playing: Some(true),
            ..Default::default()
        }),
        &["stored:cursor", "stored:another", "frame-step:another"],
    );
    check_successor(
        Action::Seek(proto::SeekStoredMedia {
            stored_media_id: "cursor".into(),
            ..Default::default()
        }),
        &["stored:another", "frame-step:another"],
    );
}
