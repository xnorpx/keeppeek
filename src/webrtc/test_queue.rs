use super::*;

pub struct ApiEventQueue {
    control: Arc<ApiSessionControl>,
    receiver: Receiver<ApiSessionCommand>,
}

impl ApiEventQueue {
    pub(crate) fn new(webrtc: &WebRtc, session_id: SessionId, capacity: usize) -> Self {
        assert!((1..=API_DATA_QUEUE_CAPACITY).contains(&capacity));
        let (data_tx, receiver) = bounded(capacity);
        let control = Arc::new(ApiSessionControl {
            session_id,
            inner: Arc::clone(&webrtc.live.inner),
            recording_demand: None,
            poller: Arc::new(Poller::new().unwrap()),
            shutdown: Arc::new(AtomicBool::new(false)),
            completion: SessionCompletion::default(),
            control_handler: Arc::new(RwLock::new(None)),
            data_tx,
            pending_event_bytes: Arc::new(AtomicUsize::new(0)),
            pending_event_count: Arc::new(AtomicUsize::new(0)),
            media_camera_ips: Mutex::new(HashSet::new()),
            background_operation_in_flight: Arc::new(AtomicBool::new(false)),
        });
        webrtc
            .live
            .inner
            .sessions
            .insert_api(session_id, Arc::clone(&control));
        Self { control, receiver }
    }

    pub(crate) fn drain(&self) -> Vec<crate::api::proto::Notification> {
        self.drain_with_control_capacity(API_CONTROL_NOTIFICATION_MAX_BYTES)
    }

    pub(crate) fn drain_with_control_capacity(
        &self,
        remaining_bytes: usize,
    ) -> Vec<crate::api::proto::Notification> {
        assert!(remaining_bytes <= API_CONTROL_NOTIFICATION_MAX_BYTES);
        let mut media = ApiMediaRuntime {
            control_notification_bytes: API_CONTROL_NOTIFICATION_MAX_BYTES - remaining_bytes,
            ..Default::default()
        };
        drain_api_session_commands(&self.receiver, &mut media);
        media.control_notifications.into_iter().collect()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.control.shutdown.load(Ordering::Acquire)
    }
}

impl Drop for ApiEventQueue {
    fn drop(&mut self) {
        self.control.close();
        self.control
            .inner
            .sessions
            .remove_api(self.control.session_id);
    }
}
