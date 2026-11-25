use super::*;
use crate::api::proto as api;

#[test]
fn shared_feedback_poll_hook_observes_an_actual_str0m_event() {
    let mut fixture = SharedFixture::new(&[StreamQuality::Auto]);
    let mut handshake = ApiHandshake::new();
    handshake.media = fixture.media;
    let emitted = handshake.observe_initial_feedback();
    fixture.media = handshake.media;
    assert!(emitted > 0);
    assert!(
        fixture.allocation(0) > 0,
        "the shared poll hook discarded the BWE event"
    );
    assert!(fixture.allocation(0) <= emitted);
}

#[test]
fn shared_feedback_pre_sctp_drain_preserves_updates_until_control_opens() {
    let mut handshake = ApiHandshake::new();
    assert!(
        handshake
            .server
            .channel(handshake.channels.control)
            .is_none()
    );
    handshake.observe_initial_feedback();
    handshake.assert_pre_sctp();
    let queued = handshake.media.control_notifications.clone();
    let queued_bytes = handshake.media.control_notification_bytes;
    assert!(!queued.is_empty(), "{}", handshake.diagnostics());
    assert!(queued_bytes > 0);
    handshake
        .drain_server()
        .expect("initial bandwidth feedback must not terminate a pre-SCTP API session");
    handshake.assert_pre_sctp();
    assert_eq!(handshake.media.control_notifications, queued);
    assert_eq!(handshake.media.control_notification_bytes, queued_bytes);
    handshake.receive_initial_notifications();
    assert!(handshake.connected);
    assert!(matches!(
        handshake
            .received
            .first()
            .and_then(|envelope| envelope.message.as_ref()),
        Some(control_envelope::Message::Notification(api::Notification {
            event: Some(api::notification::Event::InitialCapabilities(_))
        }))
    ));
    assert!(handshake.received.iter().skip(1).any(|envelope| matches!(
        &envelope.message,
        Some(control_envelope::Message::Notification(api::Notification {
            event: Some(api::notification::Event::ConnectionUpdate(_))
        }))
    )));
    assert!(handshake.media.control_notifications.is_empty());
    assert_eq!(handshake.media.control_notification_bytes, 0);
    assert_eq!(handshake.runtime.outbound_bytes, 0);
    assert_eq!(handshake.runtime.outbound_messages, 0);
    handshake.assert_control_close_is_terminal();
}

#[test]
fn camera_capability_delta_follows_the_initial_inventory_on_the_negotiated_control_channel() {
    let mut handshake = ApiHandshake::new();
    handshake.observe_initial_feedback();
    handshake.receive_initial_notifications();
    assert!(handshake.control.camera_updates.mark("front"));
    assert!(!handshake.control.camera_updates.mark("front"));
    assert!(!handshake.control.camera_updates.mark("private"));
    assert!(
        handshake
            .control
            .write_camera_updates(&mut handshake.server, handshake.channels.control)
            .unwrap()
    );
    for _ in 0..5_000 {
        handshake.progress().unwrap();
        if let Some(updated) = handshake.received.iter().find_map(|envelope| {
            let Some(control_envelope::Message::Notification(notice)) = &envelope.message else {
                return None;
            };
            let Some(api::notification::Event::CameraUpdated(updated)) = &notice.event else {
                return None;
            };
            Some(updated)
        }) {
            assert_eq!(updated.source_id, "front");
            let camera = updated.camera.as_ref().unwrap();
            assert_eq!(camera.source_id, "front");
            assert_eq!(camera.display_name, "after discovery");
            assert!(!handshake.control.camera_updates.has_work());
            assert_eq!(
                handshake
                    .received
                    .iter()
                    .filter(|envelope| matches!(
                        &envelope.message,
                        Some(control_envelope::Message::Notification(api::Notification {
                            event: Some(api::notification::Event::InitialCapabilities(_))
                        }))
                    ))
                    .count(),
                1
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!(
        "camera delta did not reach the negotiated control channel: {}",
        handshake.diagnostics()
    );
}

struct InitialCapabilities;

impl ControlRequestHandler for InitialCapabilities {
    fn handle(&self, request: api::Request) -> ControlDispatch {
        failed_control_dispatch(
            request.request_id,
            ErrorCode::UnsupportedRequest,
            "test handler",
        )
    }

    fn initial_capabilities(&self, _session_id: SessionId) -> Option<api::ServerCapabilities> {
        Some(api::ServerCapabilities {
            cameras: vec![api::CameraInfo {
                source_id: "front".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
    }

    fn camera_capability(
        &self,
        _session_id: SessionId,
        source_id: &str,
    ) -> Option<api::CameraInfo> {
        assert_eq!(source_id, "front");
        Some(api::CameraInfo {
            source_id: source_id.to_owned(),
            display_name: "after discovery".into(),
            ..Default::default()
        })
    }
}

struct ApiHandshake {
    server: Rtc,
    peer: Rtc,
    socket: UdpSocket,
    peer_socket: UdpSocket,
    channels: SessionChannels,
    peer_control: ChannelId,
    control: ApiSessionControl,
    media: ApiMediaRuntime,
    runtime: ApiControlRuntime,
    connected: bool,
    received: Vec<ControlEnvelope>,
    peer_ice: Option<IceConnectionState>,
    peer_channel_opens: u32,
    peer_received_packets: u32,
    server_received_packets: u32,
    peer_sent_packets: u32,
    _handler: Arc<dyn ControlRequestHandler>,
}

impl ApiHandshake {
    fn new() -> Self {
        let start = Instant::now();
        let socket = local_socket();
        let peer_socket = local_socket();
        let mut server = local_rtc(start, &socket, rtc_config().set_ice_lite(true));
        // Mutual SNAP opens SCTP from SDP before ICE; this case needs the ordinary handshake.
        let mut peer = local_rtc(start, &peer_socket, rtc_config().set_snap_enabled(false));
        let channels = configure_session_channels(&mut server);
        let mut changes = peer.sdp_api();
        changes.add_media(
            MediaKind::Video,
            str0m::media::Direction::RecvOnly,
            None,
            None,
            None,
        );
        let [control_config, reliable_config, unreliable_config] = session_channel_configs();
        let peer_control = changes.add_channel_with_config(control_config);
        changes.add_channel_with_config(reliable_config);
        changes.add_channel_with_config(unreliable_config);
        let (offer, pending) = changes.apply().unwrap();
        assert!(!offer.to_sdp_string().contains("a=sctp-init:"));
        let answer = server.sdp_api().accept_offer(offer).unwrap();
        assert!(!answer.to_sdp_string().contains("a=sctp-init:"));
        peer.sdp_api().accept_answer(pending, answer).unwrap();
        let handler: Arc<dyn ControlRequestHandler> = Arc::new(InitialCapabilities);
        let control = session_control(&handler);
        let (dispatch_tx, dispatch_rx) = bounded(1);
        Self {
            server,
            peer,
            socket,
            peer_socket,
            channels,
            peer_control,
            control,
            media: ApiMediaRuntime::default(),
            runtime: ApiControlRuntime {
                selection: Default::default(),
                control_output_ready: false,
                dispatch_tx,
                dispatch_rx,
                outbound: VecDeque::new(),
                outbound_bytes: 0,
                outbound_messages: 0,
                after_send: Vec::new(),
            },
            connected: false,
            received: Vec::new(),
            peer_ice: None,
            peer_channel_opens: 0,
            peer_received_packets: 0,
            server_received_packets: 0,
            peer_sent_packets: 0,
            _handler: handler,
        }
    }

    fn observe_initial_feedback(&mut self) -> u64 {
        self.server.bwe().reset(Bitrate::mbps(4));
        for _ in 0..5_000 {
            self.progress()
                .expect("the loopback handshake must reach SRTP");
            if let Some(estimate) = self.media.bandwidth.transport_estimate_bps {
                return estimate;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!(
            "the negotiated video did not produce native TWCC feedback: {}",
            self.diagnostics()
        );
    }

    fn assert_pre_sctp(&mut self) {
        assert!(
            !self.runtime.control_output_ready
                && self.server.channel(self.channels.control).is_none(),
            "the fixture passed its pre-SCTP capture point: {}",
            self.diagnostics()
        );
    }

    fn drain_server(&mut self) -> anyhow::Result<Instant> {
        drain_api_outputs(
            &mut self.server,
            &self.socket,
            self.channels,
            &self.control,
            &mut self.media,
            &mut self.connected,
            &mut self.runtime,
        )
    }

    fn receive_initial_notifications(&mut self) {
        self.drain_peer();
        for _ in 0..5_000 {
            self.progress()
                .expect("the API handshake must continue after initial feedback");
            if self.received.len() >= 2 {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!(
            "the bounded loopback handshake did not deliver its initial notifications: {}",
            self.diagnostics()
        );
    }

    fn progress(&mut self) -> anyhow::Result<()> {
        self.peer_received_packets +=
            u32::from(receive_or_timeout(&mut self.peer, &self.peer_socket));
        self.drain_peer();
        self.server_received_packets +=
            u32::from(receive_or_timeout(&mut self.server, &self.socket));
        self.drain_server()?;
        Ok(())
    }

    fn drain_peer(&mut self) {
        for _ in 0..128 {
            match self.peer.poll_output().unwrap() {
                Output::Transmit(transmit) => {
                    self.peer_socket
                        .send_to(&transmit.contents, transmit.destination)
                        .unwrap();
                    self.peer_sent_packets += 1;
                }
                Output::Event(Event::ChannelData(data)) if data.id == self.peer_control => {
                    assert!(data.binary);
                    self.received
                        .push(ControlEnvelope::decode(data.data.as_slice()).unwrap());
                    assert!(self.received.len() <= 32);
                }
                Output::Event(Event::IceConnectionStateChange(state)) => {
                    self.peer_ice = Some(state);
                }
                Output::Event(Event::ChannelOpen(_, _)) => {
                    self.peer_channel_opens += 1;
                }
                Output::Event(_) => {}
                Output::Timeout(_) => return,
            }
        }
        panic!("the peer output drain exceeded its bound");
    }

    fn assert_control_close_is_terminal(&mut self) {
        self.peer.direct_api().close_data_channel(self.peer_control);
        self.drain_peer();
        for _ in 0..5_000 {
            if let Err(error) = self.progress() {
                assert!(
                    error.to_string().contains("WebRTC control channel closed"),
                    "{error}"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!(
            "the API session ignored the real control-channel close: {}",
            self.diagnostics()
        );
    }

    fn diagnostics(&mut self) -> String {
        format!(
            "server_alive={} peer_alive={} server_connected={} peer_connected={} connected_event={} app_ready={} server_control={} peer_control={} peer_ice={:?} peer_opens={} peer_rx={} server_rx={} peer_tx={} queued_notifications={} queued_bytes={} control_messages={} control_bytes={} server_timer={:?} peer_timer={:?} envelopes={:?}",
            self.server.is_alive(),
            self.peer.is_alive(),
            self.server.is_connected(),
            self.peer.is_connected(),
            self.connected,
            self.runtime.control_output_ready,
            self.server.channel(self.channels.control).is_some(),
            self.peer.channel(self.peer_control).is_some(),
            self.peer_ice,
            self.peer_channel_opens,
            self.peer_received_packets,
            self.server_received_packets,
            self.peer_sent_packets,
            self.media.control_notifications.len(),
            self.media.control_notification_bytes,
            self.runtime.outbound_messages,
            self.runtime.outbound_bytes,
            self.server.last_timeout_reason(),
            self.peer.last_timeout_reason(),
            self.received,
        )
    }
}

fn session_control(handler: &Arc<dyn ControlRequestHandler>) -> ApiSessionControl {
    ApiSessionControl {
        session_id: SessionId(1),
        inner: Arc::new(Inner::default()),
        recording_demand: None,
        poller: Arc::new(Poller::new().unwrap()),
        shutdown: Arc::new(AtomicBool::new(false)),
        completion: SessionCompletion::default(),
        control_handler: Arc::new(RwLock::new(Some(Arc::downgrade(handler)))),
        data_tx: bounded(1).0,
        pending_event_bytes: Arc::new(AtomicUsize::new(0)),
        pending_event_count: Arc::new(AtomicUsize::new(0)),
        camera_updates: Default::default(),
        media_camera_ips: Mutex::new(HashSet::new()),
        background_operation_in_flight: Arc::new(AtomicBool::new(false)),
        share_inspection_in_flight: Arc::new(AtomicBool::new(false)),
    }
}

fn local_socket() -> UdpSocket {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    socket.set_nonblocking(true).unwrap();
    socket
}

fn local_rtc(start: Instant, socket: &UdpSocket, config: RtcConfig) -> Rtc {
    let mut rtc = config.build(start);
    let candidate = Candidate::host(socket.local_addr().unwrap(), "udp").unwrap();
    assert!(rtc.add_local_candidate(candidate).is_some());
    rtc
}

fn receive_or_timeout(rtc: &mut Rtc, socket: &UdpSocket) -> bool {
    let mut buffer = [0; UDP_PACKET_CAPACITY];
    match socket.recv_from(&mut buffer) {
        Ok((length, source)) => {
            let input = Input::Receive(
                Instant::now(),
                Receive {
                    proto: Protocol::Udp,
                    source,
                    destination: socket.local_addr().unwrap(),
                    contents: (&buffer[..length]).try_into().unwrap(),
                },
            );
            assert!(
                rtc.accepts(&input),
                "the loopback packet did not match this RTC"
            );
            rtc.handle_input(input).unwrap();
            true
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => {
            rtc.handle_input(Input::Timeout(Instant::now())).unwrap();
            false
        }
        Err(error) => panic!("loopback receive failed: {error}"),
    }
}
