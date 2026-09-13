use super::*;

pub struct LiveApiSession {
    pub(crate) id: SessionId,
    control: Arc<ApiSessionControl>,
    webrtc: WebRtc,
}

impl LiveApiSession {
    pub(crate) fn new(webrtc: &WebRtc) -> Self {
        let session = webrtc.accept_api_offer(test_api_offer()).unwrap();
        let control = webrtc.live.inner.sessions.api_control(session.id).unwrap();
        assert!(webrtc.has_api_session(session.id));
        Self {
            id: session.id,
            control,
            webrtc: webrtc.clone(),
        }
    }

    pub(crate) fn request(
        &self,
        handler: &dyn ControlRequestHandler,
        request: crate::api::proto::Request,
    ) -> ControlDispatch {
        let envelope = ControlEnvelope {
            message: Some(control_envelope::Message::Request(request)),
        };
        let dispatch = api_control_reply(
            true,
            &envelope.encode_to_vec(),
            Some(handler),
            &self.control,
            &mut ApiMediaRuntime::default(),
        );
        let Some(control_envelope::Message::Response(response)) = dispatch.envelope.message else {
            panic!("an API request must produce a response");
        };
        ControlDispatch {
            response,
            after_send: dispatch.after_send,
            data_messages: dispatch.data_messages,
            notifications: dispatch.notifications,
        }
    }

    pub(crate) fn data(&self, handler: &dyn ControlRequestHandler) -> ControlHandlerError {
        let message = crate::api::proto::Message {
            message: Some(crate::api::proto::message::Message::Event(
                crate::api::proto::EventMessage::default(),
            )),
        };
        self.control
            .handle_data_message(
                true,
                &message.encode_to_vec(),
                Some(handler),
                crate::api::proto::DataChannelKind::ReliableData,
            )
            .unwrap_err()
    }

    pub(crate) fn assert_closed(&self) {
        assert!(self.control.wait_for_finish(), "API worker must finish");
        assert!(!self.webrtc.has_api_session(self.id));
    }
}

impl Drop for LiveApiSession {
    fn drop(&mut self) {
        self.webrtc.close_api_session(self.id);
    }
}
