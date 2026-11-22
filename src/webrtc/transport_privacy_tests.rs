//! Exercises the negotiated socket boundary with two in-memory ICE/DTLS peers.
use super::*;
use str0m::media::Direction;

struct NegotiatedPeers {
    server: Rtc,
    browser: Rtc,
    channels: SessionChannels,
    audio_mid: Mid,
    video_mid: Mid,
    now: Instant,
    events: Vec<Event>,
}

impl NegotiatedPeers {
    fn new() -> Self {
        let now = Instant::now();
        let mut browser = rtc_config().build(now);
        let mut server = rtc_config().set_ice_lite(true).build(now);
        for (rtc, port) in [(&mut browser, 12001), (&mut server, 12002)] {
            rtc.add_local_candidate(
                Candidate::host(SocketAddr::from(([127, 0, 0, 1], port)), "udp").unwrap(),
            )
            .unwrap();
        }
        let channels = configure_session_channels(&mut server);
        let mut changes = browser.sdp_api();
        let audio_mid = changes.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
        let video_mid = changes.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
        for config in session_channel_configs() {
            changes.add_channel_with_config(config);
        }
        let (offer, pending) = changes.apply().unwrap();
        let answer = server.sdp_api().accept_offer(offer).unwrap();
        browser.sdp_api().accept_answer(pending, answer).unwrap();
        let mut pair = Self {
            server,
            browser,
            channels,
            audio_mid,
            video_mid,
            now,
            events: Vec::new(),
        };
        for _ in 0..1000 {
            pair.step();
            if pair.server.channel(channels.reliable_data).is_some()
                && pair.server.is_connected()
                && pair.browser.is_connected()
            {
                return pair;
            }
        }
        panic!("negotiated RTC fixture timed out");
    }

    fn step(&mut self) {
        self.now += Duration::from_millis(5);
        self.server.handle_input(Input::Timeout(self.now)).unwrap();
        drain_peer(&mut self.server, &mut self.browser, self.now, None);
        self.browser.handle_input(Input::Timeout(self.now)).unwrap();
        drain_peer(
            &mut self.browser,
            &mut self.server,
            self.now,
            Some(&mut self.events),
        );
    }

    fn admit(
        &mut self,
        path: CameraPath,
        permit: Option<MediaPermit>,
        media: &mut ApiMediaRuntime,
    ) {
        match path {
            CameraPath::RtpAudio => self.admit_audio(permit, media),
            CameraPath::RtpVideo => self.admit_video(permit, media),
            CameraPath::EventNotification => self.admit_event(permit, media),
            CameraPath::Reliable => self.admit_data(DataChannelTarget::Reliable, permit, media),
            CameraPath::Unreliable => self.admit_data(DataChannelTarget::Unreliable, permit, media),
        }
    }

    fn admit_audio(&mut self, permit: Option<MediaPermit>, media: &mut ApiMediaRuntime) {
        let (tx, rx) = bounded(1);
        tx.send(GuardedMedia {
            value: WebRtcAudioFrame {
                codec: WebRtcAudioCodec::G711Alaw,
                sample_rate_hz: 8000,
                channel_count: 1,
                timestamp: Some(Duration::from_secs(1)),
                received_at: self.now,
                data: Bytes::copy_from_slice(&[0xd5; 160]),
            },
            permit,
        })
        .unwrap();
        let mut track = AudioTrackRuntime {
            track_id: TrackId::parse("audio".into()).unwrap(),
            rx,
            delivery: TrackDelivery::Rtp(self.audio_mid),
        };
        assert!(
            drive_audio_track_runtime(&mut self.server, &mut track, true, &mut media.permits)
                .unwrap()
                .wrote_media
        );
    }

    fn admit_video(&mut self, permit: Option<MediaPermit>, media: &mut ApiMediaRuntime) {
        let frame = MediaFrame {
            codec: VideoCodec::H264,
            is_keyframe: true,
            timestamp: Some(Duration::from_secs(1)),
            received_at: self.now,
            data: Arc::new(MediaFrameData {
                permit,
                ..MediaFrameData::new(Bytes::from_static(&[0, 0, 0, 4, 0x65, 0x88, 0x84, 0x21]))
            }),
        };
        assert!(
            write_frame(
                &mut self.server,
                Some(self.video_mid),
                &frame,
                &mut MediaClock::default(),
                &mut media.permits
            )
            .unwrap()
        );
    }

    fn admit_event(&mut self, permit: Option<MediaPermit>, media: &mut ApiMediaRuntime) {
        media
            .enqueue_event(OutboundEventDelivery {
                permit,
                event: crate::api::proto::Event {
                    subscription_id: Some("events".into()),
                    source_id: "front".into(),
                    ..Default::default()
                },
                attachment_target: None,
                attachment_bytes: None,
                additional_attachments: Vec::new(),
                guard: EventDeliveryGuard::default(),
            })
            .unwrap();
        media
            .flush_control_notifications(|bytes| {
                Ok(self
                    .server
                    .channel(self.channels.control)
                    .unwrap()
                    .write(true, bytes)?)
            })
            .unwrap();
        assert!(media.control_notifications.is_empty());
    }

    fn admit_data(
        &mut self,
        target: DataChannelTarget,
        permit: Option<MediaPermit>,
        media: &mut ApiMediaRuntime,
    ) {
        media
            .outbound
            .push_back(QueuedApiData::Message(QueuedDataMessage {
                group: "camera".into(),
                target,
                payload: GuardedMedia {
                    value: b"authorized fixture audio".to_vec(),
                    permit,
                },
            }));
        media
            .flush_outbound(&mut self.server, self.channels)
            .unwrap();
        assert!(media.outbound.is_empty());
    }
}

#[derive(Clone, Copy, Debug)]
enum CameraPath {
    RtpAudio,
    RtpVideo,
    Reliable,
    Unreliable,
    EventNotification,
}

fn drain_peer(from: &mut Rtc, to: &mut Rtc, now: Instant, mut events: Option<&mut Vec<Event>>) {
    for _ in 0..256 {
        match from.poll_output().unwrap() {
            Output::Timeout(_) => return,
            Output::Event(event) => {
                if let Some(target) = events.as_deref_mut() {
                    target.push(event);
                }
            }
            Output::Transmit(tx) => {
                to.handle_input(Input::Receive(
                    now,
                    Receive {
                        proto: tx.proto,
                        source: tx.source,
                        destination: tx.destination,
                        contents: (&tx.contents[..]).try_into().unwrap(),
                    },
                ))
                .unwrap();
            }
        }
    }
    panic!("negotiated RTC output exceeded fixture bounds");
}

#[test]
fn negotiated_audio_video_and_reliable_payloads_reach_the_other_peer() {
    let mut pair = NegotiatedPeers::new();
    let payload = b"authorized fixture audio";
    let mut media = ApiMediaRuntime::default();
    for path in [
        CameraPath::RtpAudio,
        CameraPath::RtpVideo,
        CameraPath::Reliable,
    ] {
        pair.admit(path, None, &mut media);
    }
    for _ in 0..250 {
        pair.step();
        let audio = pair.events.iter().any(
            |e| matches!(e,Event::MediaData(m) if m.mid==pair.audio_mid && m.data.as_ref()==[0xd5;160]),
        );
        let data = pair
            .events
            .iter()
            .any(|e| matches!(e,Event::ChannelData(m) if m.data==payload));
        let video = pair.events.iter().any(|e| matches!(e,Event::MediaData(m) if m.mid==pair.video_mid && m.data.ends_with(&[0x65,0x88,0x84,0x21])));
        if audio && video && data {
            return;
        }
    }
    panic!("all admitted transport paths should deliver");
}

#[test]
fn negotiated_camera_paths_require_their_original_epoch_at_udp_handoff() {
    for path in [
        CameraPath::RtpAudio,
        CameraPath::RtpVideo,
        CameraPath::Reliable,
        CameraPath::Unreliable,
        CameraPath::EventNotification,
    ] {
        let registry = Arc::new(PrivacyRegistry::new(Default::default()).unwrap());
        let mut pair = NegotiatedPeers::new();
        let mut media = ApiMediaRuntime::default();
        pair.admit(path, registry.media_permit("front".into()), &mut media);
        // The RTC now owns the accepted media. A zero-length or absent datagram is insufficient evidence.
        for _ in 0..250 {
            pair.now += Duration::from_millis(5);
            pair.server.handle_input(Input::Timeout(pair.now)).unwrap();
            let mut tx = None;
            loop {
                match pair.server.poll_output().unwrap() {
                    Output::Timeout(_) => break,
                    Output::Event(_) => {}
                    Output::Transmit(packet) => {
                        tx = Some(packet);
                        break;
                    }
                }
            }
            let Some(packet) = tx else {
                continue;
            };
            assert!(!packet.contents.is_empty());
            let mut other = TransportPermits::default();
            other
                .remember(registry.media_permit("back".into()).as_ref())
                .unwrap();
            registry
                .replace_profile_overrides([("front".into(), true)].into())
                .unwrap();
            let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            receiver.set_nonblocking(true).unwrap();
            let destination = receiver.local_addr().unwrap();
            assert_blocked_datagram(&media.permits, &sender, &receiver, &packet.contents, path);
            let mut buf = [0; 2048];
            send_datagram(&other, &sender, &packet.contents, destination).unwrap();
            let length = receiver.recv(&mut buf).unwrap();
            assert_eq!(&buf[..length], &packet.contents[..]);
            registry
                .replace_profile_overrides(Default::default())
                .unwrap();
            assert_blocked_datagram(&media.permits, &sender, &receiver, &packet.contents, path);
            break;
        }
        assert!(registry.media_permit("front".into()).is_some());
        // Verify that at least one actual datagram reached the test above, even if it was denied.
        assert!(
            media.permits.send(|| Ok(())).is_err(),
            "{path:?} did not exercise UDP handoff"
        );
    }
}

fn assert_blocked_datagram(
    permits: &TransportPermits,
    sender: &UdpSocket,
    receiver: &UdpSocket,
    packet: &[u8],
    path: CameraPath,
) {
    let destination = receiver.local_addr().unwrap();
    assert!(
        send_datagram(permits, sender, packet, destination).is_err(),
        "{path:?}"
    );
    let mut buf = [0; 2048];
    assert_eq!(
        receiver.recv(&mut buf).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}

#[test]
fn independent_historical_delivery_remains_available_under_its_existing_access_policy() {
    let registry = Arc::new(PrivacyRegistry::default());
    registry
        .replace_profile_overrides([("front".into(), true)].into())
        .unwrap();
    let mut pair = NegotiatedPeers::new();
    let mut media = ApiMediaRuntime::default();
    pair.admit(CameraPath::Reliable, None, &mut media);
    for _ in 0..250 {
        pair.step();
        if pair.events.iter().any(|event| matches!(event,Event::ChannelData(data) if data.data==b"authorized fixture audio")) {
            assert!(media.permits.send(||Ok(())).is_ok());
            return;
        }
    }
    panic!("independently authorized stored data did not reach the viewer");
}
