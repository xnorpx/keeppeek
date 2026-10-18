use super::*;
use crate::storage::{CatalogFragment, CatalogRecording, RecordingCatalog};

struct Fixture {
    root: PathBuf,
    catalog: Option<RecordingCatalog>,
    state: ServerState,
    fragments: Vec<CatalogMediaFragment>,
    digest: [u8; 32],
}

impl Fixture {
    fn open_cursor(
        &self,
        offset_ms: i32,
    ) -> (SessionId, proto::StoredMediaState, Vec<OutboundDataMessage>) {
        let mut cameras = self.state.cameras.write().unwrap();
        cameras[0].configuration.recorded_audio.main_offset_ms = Some(900);
        cameras[0].configuration.recorded_audio.sub_offset_ms = Some(offset_ms);
        drop(cameras);
        let (cursor, result, messages) = open_stored_media(
            &self.state,
            proto::OpenStoredMedia {
                stored_media_id: "cursor".into(),
                source_id: "127.0.0.1".into(),
                stream_id: "main".into(),
                timestamp: Some(millis_timestamp(10_000)),
                mode: proto::StoredMediaMode::Playback as i32,
                playing: true,
                playback_rate: 1.0,
                media_channel: proto::DataChannelKind::ReliableData as i32,
                max_buffer_duration: Some(millis_duration(1)),
                ..Default::default()
            },
        )
        .unwrap();
        let session = SessionId::from_u64(913);
        self.state
            .stored_media_cursors
            .lock()
            .unwrap()
            .insert((session, "cursor".into()), cursor);
        (session, result, messages)
    }

    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("corrected-batch-{}", uuid::Uuid::new_v4()));
        let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
        write_recording(&root, &catalog.handle());
        let fragments = catalog
            .handle()
            .media_fragments_in_range("front-door/sub", 0, 20_000)
            .unwrap();
        assert_eq!(fragments.len(), 2);
        let digest = Sha256::digest(std::fs::read(root.join("source.mp4")).unwrap()).into();
        let mut state = super::super::tests::media_test_state();
        state.catalog = Some(catalog.handle());
        Self {
            root,
            catalog: Some(catalog),
            state,
            fragments,
            digest,
        }
    }

    fn request(&self) -> StoredMediaBatchRequest<'static> {
        StoredMediaBatchRequest {
            stored_media_id: "corrected",
            source_id: "127.0.0.1",
            stream_id: "main",
            recording_stream_id: "front-door/sub",
            requested_time_ms: 10_000,
            end_time_ms: None,
            mode: proto::StoredMediaMode::Playback,
            media_target: DataChannelTarget::Reliable,
            max_buffer_ms: 10_000,
            generation: 7,
            identifiers: StoredMediaIdentifiers::default(),
        }
    }

    fn batch(&self, index: usize, offset_ms: i32) -> Result<StoredMediaBatch, ControlCommandError> {
        batch(
            &self.state,
            &self.request(),
            &self.fragments[index],
            self.fragments[index].start_ms + 1,
            10_000,
            &timing(offset_ms),
        )
    }

    fn assert_released(&self) {
        assert_eq!(self.state.recorded_audio_workers.load(Ordering::Acquire), 0);
        let digest: [u8; 32] =
            Sha256::digest(std::fs::read(self.root.join("source.mp4")).unwrap()).into();
        assert_eq!(digest, self.digest);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.catalog.take().unwrap().shutdown();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn timing(offset_ms: i32) -> proto::RecordedAudioTiming {
    proto::RecordedAudioTiming {
        applied_offset_ms: offset_ms,
        ..Default::default()
    }
}

#[test]
fn cursor_refills_keep_the_captured_actual_stream_policy_origin_and_monotonic_ids() {
    for offset in [0, 200] {
        let fixture = Fixture::new();
        let (session, opened, messages) = fixture.open_cursor(offset);
        assert_eq!(
            opened.recorded_audio.as_ref().unwrap().applied_offset_ms,
            offset
        );
        fixture.state.cameras.write().unwrap()[0]
            .configuration
            .recorded_audio
            .sub_offset_ms = Some(-400);
        let (refilled, more) = refill_stored_media(
            &fixture.state,
            session,
            proto::RefillStoredMedia {
                stored_media_id: "cursor".into(),
                playback_time: Some(millis_timestamp(11_000)),
            },
        )
        .unwrap();
        assert_eq!(refilled.generation, opened.generation);
        assert_eq!(refilled.fragment_time, opened.fragment_time);
        assert_eq!(refilled.recorded_audio, opened.recorded_audio);
        let ids = |messages: &[OutboundDataMessage]| {
            messages
                .iter()
                .filter_map(|message| {
                    let Some(proto::message::Message::StoredMedia(message)) =
                        &message.message.message
                    else {
                        return None;
                    };
                    match message.message.as_ref()? {
                        proto::stored_media_message::Message::Fragment(fragment) => {
                            Some((fragment.initialization_id, fragment.sequence))
                        }
                        _ => None,
                    }
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&messages), vec![(1, 1)]);
        assert_eq!(ids(&more), vec![(2, 2)]);
        fixture.assert_released();
    }
}

#[test]
fn preview_seek_and_configured_reset_rotate_generations_without_saving_or_changing_rate() {
    let fixture = Fixture::new();
    let (session, opened, _) = fixture.open_cursor(200);
    let preview = |selection| proto::SetStoredMediaPlayback {
        stored_media_id: "cursor".into(),
        recorded_audio_preview: Some(proto::RecordedAudioPreview {
            selection: Some(selection),
        }),
        ..Default::default()
    };
    let (applied, _) = set_stored_media_playback(
        &fixture.state,
        session,
        proto::SetStoredMediaPlayback {
            playing: Some(false),
            playback_rate: Some(1.5),
            ..preview(proto::recorded_audio_preview::Selection::OffsetMs(-200))
        },
    )
    .unwrap();
    assert_eq!(applied.generation, opened.generation + 1);
    assert_eq!(
        applied.recorded_audio.as_ref().unwrap().applied_offset_ms,
        -200
    );
    assert!(applied.recorded_audio.as_ref().unwrap().preview);
    assert!(!applied.playing);
    assert_eq!(applied.playback_rate, 1.5);
    let (sought, _) = seek_stored_media(
        &fixture.state,
        session,
        proto::SeekStoredMedia {
            stored_media_id: "cursor".into(),
            timestamp: Some(millis_timestamp(11_001)),
        },
    )
    .unwrap();
    assert_eq!(sought.generation, applied.generation + 1);
    assert_eq!(sought.fragment_time, Some(millis_timestamp(11_000)));
    let (reset, _) = set_stored_media_playback(
        &fixture.state,
        session,
        preview(proto::recorded_audio_preview::Selection::UseConfigured(
            true,
        )),
    )
    .unwrap();
    assert_eq!(reset.generation, sought.generation + 1);
    assert_eq!(
        reset.recorded_audio.as_ref().unwrap().applied_offset_ms,
        200
    );
    assert!(!reset.recorded_audio.as_ref().unwrap().preview);
    assert_eq!(reset.playback_rate, 1.5);
    assert!(!reset.playing);
    assert_eq!(
        fixture
            .state
            .camera("127.0.0.1")
            .unwrap()
            .configuration
            .recorded_audio
            .sub_offset_ms,
        Some(200)
    );
    fixture.assert_released();
}

#[test]
fn rejected_preview_does_not_mutate_cursor_policy_generation_or_playback_controls() {
    let fixture = Fixture::new();
    let (session, opened, _) = fixture.open_cursor(200);
    let error = set_stored_media_playback(
        &fixture.state,
        session,
        proto::SetStoredMediaPlayback {
            stored_media_id: "cursor".into(),
            playing: Some(false),
            playback_rate: Some(2.0),
            recorded_audio_preview: Some(proto::RecordedAudioPreview {
                selection: Some(proto::recorded_audio_preview::Selection::OffsetMs(5_001)),
            }),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest);
    assert_eq!(
        stored_media_cursor_state(&fixture.state, session, "cursor").unwrap(),
        opened
    );
    fixture.assert_released();
}

fn track_configurations() -> Vec<mp4::TrackConfig> {
    vec![
        mp4::TrackConfig {
            track_type: mp4::TrackType::Video,
            timescale: 90_000,
            language: "und".into(),
            media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                width: 320,
                height: 240,
                seq_param_set: vec![0x67, 0x42, 0, 0x1e, 0xe9, 1, 0x40, 0x7b, 0x20],
                pic_param_set: vec![0x68, 0xce, 6, 0xe2],
            }),
        },
        mp4::TrackConfig {
            track_type: mp4::TrackType::Audio,
            timescale: 48_000,
            language: "und".into(),
            media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
                bitrate: 64_000,
                profile: mp4::AudioObjectType::AacLowComplexity,
                freq_index: mp4::SampleFreqIndex::Freq48000,
                chan_conf: mp4::ChannelConfig::Mono,
            }),
        },
    ]
}

fn write_recording(root: &Path, catalog: &RecordingCatalogHandle) {
    let path = root.join("source.mp4");
    let mut writer = mp4::FragmentedMp4Writer::write_start(
        File::create(&path).unwrap(),
        &mp4::Mp4Config {
            major_brand: "iso6".parse().unwrap(),
            minor_version: 1,
            compatible_brands: vec!["iso6".parse().unwrap()],
            timescale: 1_000,
        },
        &track_configurations(),
    )
    .unwrap();
    let initialization = writer.initialization();
    let fragments = write_periods(&mut writer);
    drop(writer);
    catalog
        .upsert_recording(CatalogRecording {
            id: "recording".into(),
            stream_id: "front-door/sub".into(),
            source_id: Some("127.0.0.1".into()),
            logical_stream_id: Some("sub".into()),
            started_at_ms: 10_000,
            ended_at_ms: Some(12_000),
            path: path.to_string_lossy().into_owned(),
            init_offset: initialization.offset,
            init_len: initialization.size,
            finalized: true,
        })
        .unwrap();
    for (index, fragment) in fragments.into_iter().enumerate() {
        catalog
            .insert_fragment(CatalogFragment {
                recording_id: "recording".into(),
                sequence: u64::from(fragment.sequence_number),
                start_ms: 10_000 + i64::try_from(index).unwrap() * 1_000,
                duration_ms: 1_000,
                byte_offset: fragment.range.offset,
                byte_len: fragment.range.size,
                random_access: true,
            })
            .unwrap();
    }
}

fn write_periods(writer: &mut mp4::FragmentedMp4Writer<File>) -> Vec<mp4::Mp4FragmentInfo> {
    let mut fragments = Vec::new();
    for (index, packets) in [[(900_u64, 1_u8), (980, 2)], [(1_000, 3), (1_080, 4)]]
        .iter()
        .enumerate()
    {
        writer
            .write_sample(
                1,
                mp4::Mp4Sample {
                    start_time: u64::try_from(index).unwrap() * 90_000,
                    duration: 90_000,
                    rendering_offset: 450,
                    is_sync: true,
                    bytes: bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65]),
                },
            )
            .unwrap();
        for &(start_ms, marker) in packets {
            writer
                .write_sample(
                    2,
                    mp4::Mp4Sample {
                        start_time: start_ms * 48,
                        duration: 960,
                        rendering_offset: 0,
                        is_sync: true,
                        bytes: vec![marker, 0x55, 0xaa].into(),
                    },
                )
                .unwrap();
        }
        fragments.push(writer.flush_fragment().unwrap().unwrap());
    }
    fragments
}

fn emitted_audio(batch: &StoredMediaBatch) -> Vec<(u64, u8)> {
    let mut initialization = Vec::new();
    let mut payload = Vec::new();
    for outbound in &batch.messages {
        let Some(proto::message::Message::StoredMedia(message)) = &outbound.message.message else {
            panic!("unexpected batch message")
        };
        match message.message.as_ref().unwrap() {
            proto::stored_media_message::Message::Initialization(value) => {
                assert_eq!(value.generation, 7);
                initialization.extend_from_slice(&value.payload);
            }
            proto::stored_media_message::Message::Fragment(value) => {
                assert_eq!(
                    value.start_time,
                    Some(millis_timestamp(batch.fragment_time_ms))
                );
                assert_eq!(value.duration.as_ref().unwrap().seconds, 1);
                payload.extend_from_slice(&value.payload);
            }
            _ => panic!("unexpected corrected playback message"),
        }
    }
    initialization.extend_from_slice(&payload);
    let size = u64::try_from(initialization.len()).unwrap();
    let mut reader = mp4::Mp4Reader::read_header(Cursor::new(initialization), size).unwrap();
    let (&id, track) = reader
        .tracks()
        .iter()
        .find(|(_, track)| track.track_type().unwrap() == mp4::TrackType::Audio)
        .unwrap();
    (1..=track.sample_count())
        .map(|index| {
            let sample = reader.read_sample(id, index).unwrap().unwrap();
            (sample.start_time, sample.bytes[0])
        })
        .collect()
}

#[test]
fn corrected_batch_reads_lookahead_through_the_full_last_gop() {
    let fixture = Fixture::new();
    let output = fixture.batch(0, -200).unwrap();
    assert_eq!(output.fragment_time_ms, 10_000);
    assert_eq!(output.delivered_through_ms, 11_000);
    assert_eq!(
        emitted_audio(&output),
        vec![(33_600, 1), (37_440, 2), (38_400, 3), (42_240, 4)]
    );
    assert_eq!(
        output.audio_timing.unwrap().first_audio_pts_us,
        Some(900_000)
    );
    fixture.assert_released();
}

#[test]
fn corrected_batch_delayed_audio_reads_previous_gop_on_the_same_cursor_clock() {
    let fixture = Fixture::new();
    let output = fixture.batch(1, 200).unwrap();
    assert_eq!(output.fragment_time_ms, 11_000);
    assert_eq!(output.delivered_through_ms, 12_000);
    assert_eq!(
        emitted_audio(&output),
        vec![(52_800, 1), (56_640, 2), (57_600, 3), (61_440, 4)]
    );
    fixture.assert_released();
}

#[test]
fn correction_admission_is_bounded_and_permits_are_released() {
    let fixture = Fixture::new();
    let first = Permit::acquire(&fixture.state.recorded_audio_workers).unwrap();
    let second = Permit::acquire(&fixture.state.recorded_audio_workers).unwrap();
    let error = fixture.batch(0, -200).err().unwrap();
    assert_eq!(error.code, proto::ErrorCode::Rejected);
    assert_eq!(error._http_status, 429);
    drop(first);
    assert!(fixture.batch(0, -200).is_ok());
    drop(second);
    fixture.assert_released();
}

#[test]
fn maintenance_privacy_and_extent_failures_release_admission() {
    let fixture = Fixture::new();
    fixture
        .state
        .maintenance_active
        .store(true, Ordering::Release);
    assert!(fixture.batch(0, -200).is_err());
    fixture
        .state
        .maintenance_active
        .store(false, Ordering::Release);
    fixture
        .state
        .privacy
        .replace_profile_overrides(BTreeMap::from([("127.0.0.1".into(), true)]))
        .unwrap();
    assert!(fixture.batch(0, -200).is_err());
    fixture
        .state
        .privacy
        .replace_profile_overrides(BTreeMap::new())
        .unwrap();
    assert!(
        batch(
            &fixture.state,
            &fixture.request(),
            &fixture.fragments[0],
            60_000,
            10_000,
            &timing(-200)
        )
        .is_err()
    );
    fixture.assert_released();
}

#[test]
fn bounded_catalog_queries_reject_dense_windows_without_changing_ordinary_queries() {
    let fixture = Fixture::new();
    let fragment = &fixture.fragments[0];
    let catalog = fixture.catalog.as_ref().unwrap().handle();
    for sequence in 3..=33 {
        catalog
            .insert_fragment(CatalogFragment {
                recording_id: fragment.recording_id.clone(),
                sequence,
                start_ms: fragment.start_ms,
                duration_ms: fragment.duration_ms,
                byte_offset: fragment.byte_offset,
                byte_len: fragment.byte_len,
                random_access: true,
            })
            .unwrap();
    }
    assert_eq!(
        catalog
            .media_fragments_in_range("front-door/sub", 10_000, 12_000)
            .unwrap()
            .len(),
        33
    );
    assert!(
        catalog
            .leased_media_fragments_in_range_bounded("front-door/sub", 10_000, 12_000, 32)
            .is_err()
    );
    assert!(fixture.batch(0, -200).is_err());
    fixture.assert_released();
}

#[test]
fn corrected_refills_keep_identifiers_monotonic_within_the_same_generation() {
    let fixture = Fixture::new();
    let mut request = fixture.request();
    request.identifiers = StoredMediaIdentifiers {
        initialization_id: 40,
        sequence: 100,
    };
    let first = batch(
        &fixture.state,
        &request,
        &fixture.fragments[0],
        10_001,
        10_000,
        &timing(-200),
    )
    .unwrap();
    assert_eq!(
        first.identifiers,
        StoredMediaIdentifiers {
            initialization_id: 41,
            sequence: 101
        }
    );
    request.identifiers = first.identifiers;
    let following = batch(
        &fixture.state,
        &request,
        &fixture.fragments[1],
        11_001,
        10_000,
        &timing(-200),
    )
    .unwrap();
    assert_eq!(
        following.identifiers,
        StoredMediaIdentifiers {
            initialization_id: 42,
            sequence: 102
        }
    );
    assert_identifiers(&first);
    assert_identifiers(&following);
    fixture.assert_released();
}

fn assert_identifiers(output: &StoredMediaBatch) {
    let mut initialization_count = 0;
    let mut fragment_count = 0;
    for message in &output.messages {
        let Some(proto::message::Message::StoredMedia(message)) = &message.message.message else {
            panic!("unexpected message")
        };
        match message.message.as_ref().unwrap() {
            proto::stored_media_message::Message::Initialization(value) => {
                initialization_count += 1;
                assert_eq!(value.generation, 7);
                assert_eq!(
                    value.initialization_id,
                    output.identifiers.initialization_id
                );
            }
            proto::stored_media_message::Message::Fragment(value) => {
                fragment_count += 1;
                assert_eq!(value.generation, 7);
                assert_eq!(
                    value.initialization_id,
                    output.identifiers.initialization_id
                );
                assert_eq!(value.sequence, output.identifiers.sequence);
            }
            _ => panic!("unexpected corrected message"),
        }
    }
    assert_eq!((initialization_count, fragment_count), (1, 1));
}
