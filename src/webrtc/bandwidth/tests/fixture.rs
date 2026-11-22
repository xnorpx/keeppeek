//! Replays shared feedback and source admission with a simulated clock.

use super::*;
use crate::api::proto as api;
use str0m::media::Direction;

pub(super) struct SharedFixture {
    pub(super) start: Instant,
    pub(super) media: ApiMediaRuntime,
    pub(super) rtc: Rtc,
    pub(super) session: ApiSessionControl,
    publisher: Publisher,
}

impl SharedFixture {
    pub(super) fn new(qualities: &[StreamQuality]) -> Self {
        let specs: Vec<_> = qualities.iter().map(|quality| (*quality, true)).collect();
        Self::with_substreams(&specs)
    }

    pub(super) fn with_substreams(specs: &[(StreamQuality, bool)]) -> Self {
        let variants: Vec<_> = specs
            .iter()
            .map(|(quality, sub)| (*quality, true, *sub))
            .collect();
        Self::with_variants(&variants)
    }

    pub(super) fn with_variants(specs: &[(StreamQuality, bool, bool)]) -> Self {
        Self::with_offer_config(specs, rtc_config().enable_h265(false))
    }

    pub(super) fn with_offer_config(
        specs: &[(StreamQuality, bool, bool)],
        offer_config: RtcConfig,
    ) -> Self {
        assert!(!specs.is_empty() && specs.len() <= 8);
        let start = Instant::now();
        let inner = Arc::new(Inner::default());
        let (rtc, mids, audio_mid) = negotiated_rtc(start, specs.len(), offer_config);
        let session = ApiSessionControl {
            session_id: SessionId(1),
            inner: inner.clone(),
            recording_demand: None,
            poller: Arc::new(Poller::new().unwrap()),
            shutdown: Arc::new(AtomicBool::new(false)),
            completion: SessionCompletion::default(),
            control_handler: Arc::new(RwLock::new(None)),
            data_tx: bounded(1).0,
            pending_event_bytes: Arc::new(AtomicUsize::new(0)),
            pending_event_count: Arc::new(AtomicUsize::new(0)),
            media_camera_ips: Mutex::new(HashSet::new()),
            background_operation_in_flight: Arc::new(AtomicBool::new(false)),
        };
        let mut fixture = Self {
            start,
            media: ApiMediaRuntime {
                available_video_mids: mids,
                available_audio_mids: vec![audio_mid],
                ..Default::default()
            },
            rtc,
            session,
            publisher: Publisher { inner },
        };
        for (index, (quality, has_main, has_sub)) in specs.iter().copied().enumerate() {
            fixture.subscribe_variant(index, quality, has_main, has_sub, None);
        }
        fixture.publish(0, true);
        fixture.set_source_demands();
        fixture
    }

    pub(super) fn subscribe(
        &mut self,
        index: usize,
        quality: StreamQuality,
        has_sub: bool,
    ) -> api::SubscriptionResult {
        self.subscribe_variant(index, quality, true, has_sub, None)
    }

    pub(super) fn subscribe_exact(
        &mut self,
        index: usize,
        stream: StreamKind,
    ) -> api::SubscriptionResult {
        let quality = match stream {
            StreamKind::Main => StreamQuality::High,
            StreamKind::Sub => StreamQuality::Low,
        };
        self.subscribe_variant(index, quality, true, true, Some(stream))
    }

    fn subscribe_variant(
        &mut self,
        index: usize,
        quality: StreamQuality,
        has_main: bool,
        has_sub: bool,
        exact: Option<StreamKind>,
    ) -> api::SubscriptionResult {
        let requested_quality = if exact.is_some() {
            StreamQuality::Auto
        } else {
            quality
        };
        let selected_stream = if has_main && (quality == StreamQuality::High || !has_sub) {
            StreamKind::Main
        } else {
            StreamKind::Sub
        };
        let codec = self
            .session
            .inner
            .sources
            .lock()
            .unwrap()
            .get(&Source {
                camera_ip: camera_ip(index),
                stream: selected_stream,
            })
            .and_then(|source| source.keyframe.as_ref())
            .map_or("h264", |frame| match frame.codec {
                VideoCodec::H264 => "h264",
                VideoCodec::H265 => "h265",
            });
        let request = api::SubscribeMedia {
            subscription_id: format!("camera-{index}"),
            source_session_id: format!("camera:camera-{index}"),
            kind: api::MediaKind::Video as i32,
            requested_delivery_transport: api::DeliveryTransport::Rtp as i32,
            video_quality: match requested_quality {
                StreamQuality::Auto => api::VideoQuality::Auto,
                StreamQuality::High => api::VideoQuality::High,
                StreamQuality::Low => api::VideoQuality::Low,
            } as i32,
            variant_id: exact.map(|stream| stream.to_string()).unwrap_or_default(),
        };
        self.media
            .subscribe(
                &self.session,
                &request,
                MediaSubscriptionPlan {
                    media_kind: api::MediaKind::Video,
                    source_session_id: request.source_session_id.clone(),
                    camera_ip: camera_ip(index),
                    has_main_stream: has_main,
                    has_sub_stream: has_sub,
                    recording_label: format!("camera-{index}"),
                    quality,
                    delivery_transport: api::DeliveryTransport::Rtp,
                    codec: api::CodecDescriptor {
                        name: codec.to_owned(),
                        parameters: HashMap::new(),
                    },
                    format: api::MediaDataFormat::default(),
                    selected_variant_id: selected_stream.to_string(),
                },
            )
            .unwrap()
    }

    pub(super) fn feedback(&mut self, bitrate_bps: u64, time_ms: u64) {
        let now = self.start + Duration::from_millis(time_ms);
        observe_api_output(
            &mut self.rtc,
            &mut self.media,
            &Output::Event(Event::EgressBitrateEstimate(BweKind::Twcc(Bitrate::bps(
                bitrate_bps,
            )))),
            now,
        );
    }

    pub(super) fn remb(&mut self, mid: Mid, bitrate_bps: u64, time_ms: u64) {
        let now = self.start + Duration::from_millis(time_ms);
        observe_api_output(
            &mut self.rtc,
            &mut self.media,
            &Output::Event(Event::EgressBitrateEstimate(BweKind::Remb(
                mid,
                Bitrate::bps(bitrate_bps),
            ))),
            now,
        );
    }

    pub(super) fn allocation(&self, index: usize) -> u64 {
        self.media.tracks[index]
            .runtime
            .subscription
            .control
            .as_ref()
            .unwrap()
            .estimated_bitrate_bps
            .load(Ordering::Acquire)
    }

    pub(super) fn active_stream(&self, index: usize) -> StreamKind {
        self.media.tracks[index]
            .runtime
            .subscription
            .active_source
            .stream
    }

    pub(super) fn assert_subscriber_bounds(&self) {
        let sources = self.session.inner.sources.lock().unwrap();
        for track in &self.media.tracks {
            let count = sources
                .values()
                .flat_map(|source| &source.subscribers)
                .filter(|sender| sender.track_id.as_ref() == Some(&track.runtime.track_id))
                .count();
            assert!(
                (1..=2).contains(&count),
                "a track has {count} source subscribers"
            );
            assert!(track.runtime.rx.len() <= FRAME_QUEUE_CAPACITY);
        }
    }

    fn publish(&self, time_ms: u64, is_keyframe: bool) {
        self.publish_withheld(time_ms, is_keyframe, None);
    }

    fn publish_withheld(&self, time_ms: u64, is_keyframe: bool, withheld: Option<Source>) {
        let received_at = self.start + Duration::from_millis(time_ms);
        for track in &self.media.tracks {
            let subscription = &track.runtime.subscription;
            for source in [Some(subscription.high_source), subscription.low_source]
                .into_iter()
                .flatten()
            {
                let is_keyframe = is_keyframe && Some(source) != withheld;
                self.publisher.publish(
                    source,
                    VideoCodec::H264,
                    is_keyframe,
                    received_at,
                    Some(Duration::from_millis(time_ms)),
                    if is_keyframe {
                        Bytes::from_static(&[
                            0, 0, 0, 4, 0x67, 0x42, 0x00, 0x1f, 0, 0, 0, 2, 0x65, 0x88,
                        ])
                    } else {
                        Bytes::from_static(&[0, 0, 0, 2, 0x41, 0x88])
                    },
                );
            }
        }
    }

    fn set_source_demands(&self) {
        // Source demand is a controlled input. These payloads do not measure the ingest estimator.
        let mut sources = self.session.inner.sources.lock().unwrap();
        for (source, state) in &mut *sources {
            state.bitrate.estimate_bps = Some(match source.stream {
                StreamKind::Main => 4_000_000,
                StreamKind::Sub => 500_000,
            });
        }
    }

    pub(super) fn prime(&mut self) {
        self.drive(0);
        self.advance(1, true);
        self.advance(2, true);
        assert!(
            self.media
                .tracks
                .iter()
                .all(|track| track.runtime.keyframe_gate.has_live_gop())
        );
    }

    pub(super) fn advance(&mut self, time_ms: u64, keyframe: bool) {
        self.advance_withheld(time_ms, keyframe, None);
    }

    pub(super) fn advance_withheld(
        &mut self,
        time_ms: u64,
        keyframe: bool,
        withheld: Option<Source>,
    ) {
        self.drive(time_ms);
        self.assert_subscriber_bounds();
        self.publish_withheld(time_ms, keyframe, withheld);
        self.set_source_demands();
        self.drive(time_ms);
    }

    fn drive(&mut self, time_ms: u64) {
        // ponytail: This negotiated-only fixture tests admission. A connected peer must test delivery.
        let now = self.start + Duration::from_millis(time_ms);
        self.media.refresh_bandwidth(&mut self.rtc, now);
        let mut streams = Vec::new();
        for track in &mut self.media.tracks {
            let result = drive_track_runtime(&mut self.rtc, &mut track.runtime, true, now).unwrap();
            assert!(result.data_messages.is_empty());
            assert!(result.notifications.is_empty());
            if let Some(stream) = result.applied_stream {
                streams.push((track.runtime.track_id.clone(), stream));
            }
        }
        for (track_id, stream) in streams {
            self.media.enqueue_stream_state(&track_id, stream);
        }
        drain_rtc(&mut self.rtc, now);
    }

    pub(super) fn take_stream_states(&mut self) -> Vec<api::SubscriptionStreamState> {
        let mut states = Vec::new();
        self.media
            .flush_control_notifications(|payload| {
                let envelope = ControlEnvelope::decode(payload).unwrap();
                let Some(control_envelope::Message::Notification(notification)) = envelope.message
                else {
                    panic!("stream notification must use a control notification envelope");
                };
                match notification.event {
                    Some(api::notification::Event::SubscriptionStreamState(state)) => {
                        states.push(state);
                    }
                    Some(api::notification::Event::ConnectionUpdate(_)) => {}
                    _ => panic!("unexpected control notification in the source admission trace"),
                }
                Ok(true)
            })
            .unwrap();
        states
    }

    pub(super) fn mid(&self, index: usize) -> Mid {
        self.media.tracks[index].runtime.delivery.mid().unwrap()
    }

    pub(super) fn source_demand(&self, index: usize) -> u64 {
        let subscription = &self.media.tracks[index].runtime.subscription;
        subscription
            .source_bitrate(subscription.active_source)
            .as_u64()
    }

    pub(super) fn subscriber_count(&self, index: usize) -> usize {
        let track_id = &self.media.tracks[index].runtime.track_id;
        self.session
            .inner
            .sources
            .lock()
            .unwrap()
            .values()
            .flat_map(|source| &source.subscribers)
            .filter(|sender| sender.track_id.as_ref() == Some(track_id))
            .count()
    }

    pub(super) fn queue_high_water(&self) -> usize {
        self.session.inner.queue_high_water.load(Ordering::Relaxed)
    }

    pub(super) fn replace_quality(&mut self, index: usize, quality: StreamQuality) {
        let has_sub = self.media.tracks[index]
            .runtime
            .subscription
            .low_source
            .is_some();
        self.subscribe(index, quality, has_sub);
    }

    pub(super) fn remove(&mut self, index: usize) {
        let id = self.media.tracks[index].runtime.track_id.0.clone();
        self.media.unsubscribe(&self.session, &[id]);
    }

    pub(super) fn subscribe_audio(&mut self, transport: api::DeliveryTransport) -> String {
        assert!(matches!(
            transport,
            api::DeliveryTransport::Rtp | api::DeliveryTransport::ReliableData
        ));
        let id = if transport == api::DeliveryTransport::Rtp {
            "audio-rtp"
        } else {
            "audio-reliable"
        };
        let request = api::SubscribeMedia {
            subscription_id: id.to_owned(),
            source_session_id: "camera:camera-0".to_owned(),
            kind: api::MediaKind::Audio as i32,
            requested_delivery_transport: transport as i32,
            variant_id: "main".to_owned(),
            ..Default::default()
        };
        self.media
            .subscribe(
                &self.session,
                &request,
                MediaSubscriptionPlan {
                    media_kind: api::MediaKind::Audio,
                    source_session_id: request.source_session_id.clone(),
                    camera_ip: camera_ip(0),
                    has_main_stream: true,
                    has_sub_stream: false,
                    recording_label: "camera-0".to_owned(),
                    quality: StreamQuality::High,
                    delivery_transport: transport,
                    codec: api::CodecDescriptor {
                        name: if transport == api::DeliveryTransport::Rtp {
                            "pcmu"
                        } else {
                            "aac"
                        }
                        .to_owned(),
                        parameters: HashMap::new(),
                    },
                    format: api::MediaDataFormat {
                        format: Some(api::media_data_format::Format::Audio(
                            api::AudioDataFormat {
                                sample_rate_hz: 8_000,
                                channel_count: 1,
                                decoder_config: Vec::new(),
                            },
                        )),
                    },
                    selected_variant_id: "main".to_owned(),
                },
            )
            .unwrap();
        request.subscription_id
    }

    pub(super) fn subscribe_fixed_data(
        &mut self,
        index: usize,
        stream: StreamKind,
    ) -> Result<api::SubscriptionResult, ControlHandlerError> {
        let request = api::SubscribeMedia {
            subscription_id: format!("data-{index}"),
            source_session_id: format!("camera:camera-{index}"),
            kind: api::MediaKind::Video as i32,
            requested_delivery_transport: api::DeliveryTransport::ReliableData as i32,
            variant_id: stream.to_string(),
            ..Default::default()
        };
        self.media.subscribe(
            &self.session,
            &request,
            MediaSubscriptionPlan {
                media_kind: api::MediaKind::Video,
                source_session_id: request.source_session_id.clone(),
                camera_ip: camera_ip(index),
                has_main_stream: true,
                has_sub_stream: true,
                recording_label: format!("camera-{index}"),
                quality: match stream {
                    StreamKind::Main => StreamQuality::High,
                    StreamKind::Sub => StreamQuality::Low,
                },
                delivery_transport: api::DeliveryTransport::ReliableData,
                codec: api::CodecDescriptor {
                    name: "h264".to_owned(),
                    parameters: HashMap::new(),
                },
                format: api::MediaDataFormat::default(),
                selected_variant_id: stream.to_string(),
            },
        )
    }

    pub(super) fn close(&mut self) {
        let mids: Vec<_> = self
            .media
            .tracks
            .iter()
            .filter_map(|track| track.runtime.delivery.mid())
            .collect();
        let ids: Vec<_> = self
            .media
            .tracks
            .iter()
            .map(|track| track.runtime.track_id.0.clone())
            .chain(
                self.media
                    .audio_tracks
                    .iter()
                    .map(|track| track.runtime.track_id.0.clone()),
            )
            .collect();
        self.media.unsubscribe(&self.session, &ids);
        assert!(self.media.tracks.is_empty());
        assert!(self.media.audio_tracks.is_empty());
        assert!(
            self.session
                .inner
                .audio_subscribers
                .lock()
                .unwrap()
                .is_empty()
        );
        assert!(
            mids.iter()
                .all(|mid| self.media.available_video_mids.contains(mid))
        );
        assert!(
            self.session
                .inner
                .sources
                .lock()
                .unwrap()
                .values()
                .all(|source| source.subscribers.is_empty())
        );
    }
}

fn negotiated_rtc(start: Instant, count: usize, offer_config: RtcConfig) -> (Rtc, Vec<Mid>, Mid) {
    let mut offerer = offer_config.build(start);
    let mut changes = offerer.sdp_api();
    let mids = (0..count)
        .map(|_| changes.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None))
        .collect();
    let audio_mid = changes.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
    let (offer, pending) = changes.apply().unwrap();
    let mut rtc = rtc_config().build(start);
    let answer = rtc.sdp_api().accept_offer(offer).unwrap();
    offerer.sdp_api().accept_answer(pending, answer).unwrap();
    drain_rtc(&mut rtc, start);
    (rtc, mids, audio_mid)
}

fn drain_rtc(rtc: &mut Rtc, now: Instant) {
    rtc.handle_input(Input::Timeout(now)).unwrap();
    for _ in 0..256 {
        if matches!(rtc.poll_output().unwrap(), Output::Timeout(_)) {
            return;
        }
    }
    panic!("negotiated RTP fixture exceeded the output drain bound");
}

fn camera_ip(index: usize) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 0, 2, u8::try_from(index + 1).unwrap()))
}
