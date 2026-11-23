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
impl ControlRequestHandler for Handler {
    fn handle(&self, request: Request) -> ControlDispatch {
        self.started.send(request.request_id).unwrap();
        self.release.recv_timeout(Duration::from_secs(5)).unwrap();
        ControlDispatch {
            response: proto::Response {
                request_id: request.request_id,
                result: Some(control_response::Result::Ok(proto::Ok::default())),
            },
            after_send: (request.request_id == 1).then(|| {
                Box::new(|| panic!("superseded frame must never launch a transfer"))
                    as crate::webrtc::PostSendAction
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

fn check_successor(action: Action, expected_group: &str) {
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
    for group in ["stored:cursor", "frame-step:cursor"] {
        media
            .enqueue(vec![OutboundDataMessage {
                target: DataChannelTarget::Reliable,
                group: group.into(),
                message: proto::Message::default(),
            }])
            .unwrap();
    }
    let (started, received) = bounded(1);
    let (release, proceed) = bounded(0);
    let handler = Arc::new(Handler {
        started,
        release: proceed,
    });
    let step = Action::StepFrame(proto::StepStoredFrame {
        stored_media_id: "cursor".into(),
        ..Default::default()
    });
    dispatch(request(1, step), handler.clone(), &control, &mut runtime).unwrap();
    assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    let other = Action::Seek(proto::SeekStoredMedia {
        stored_media_id: "another".into(),
        ..Default::default()
    });
    dispatch(request(3, other), handler.clone(), &control, &mut runtime).unwrap();
    dispatch(request(2, action), handler, &control, &mut runtime).unwrap();
    assert!(received.is_empty());
    assert_eq!(media.outbound.len(), 2);
    release.send(()).unwrap();
    complete_worker(&runtime);
    drain_pending_control_dispatches(&control, &mut runtime, &mut media).unwrap();
    assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
    assert_eq!(media.outbound.len(), 2);
    release.send(()).unwrap();
    complete_worker(&runtime);
    drain_pending_control_dispatches(&control, &mut runtime, &mut media).unwrap();
    assert!(
        !control
            .background_operation_in_flight
            .load(Ordering::Acquire)
    );
    assert_eq!(media.outbound.len(), 1);
    assert!(
        matches!(media.outbound.front(), Some(QueuedApiData::Message(message)) if message.group == expected_group)
    );
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
    assert_eq!(replies, [(3, false), (1, false), (2, true)]);
}

#[test]
fn resume_and_cold_seek_before_frame_ready_wait_for_the_same_cursor_and_preserve_media() {
    check_successor(
        Action::SetPlayback(proto::SetStoredMediaPlayback {
            stored_media_id: "cursor".into(),
            playing: Some(true),
            ..Default::default()
        }),
        "stored:cursor",
    );
    check_successor(
        Action::Seek(proto::SeekStoredMedia {
            stored_media_id: "cursor".into(),
            ..Default::default()
        }),
        "frame-step:cursor",
    );
}
