use keeppeek::{
    cameras::{CameraConfig, CameraRecordingMode, configured_cameras, events::EventMode},
    config::load_cameras,
    keeppeek::KeepPeekLoop,
    shutdown::Shutdown,
    stats::HealthRegistry,
    storage::{
        RecordingCatalog, StorageConfig, StorageEngine,
        events::EventStore,
        metadata::{EventSource, TimelineEvent},
    },
};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use test_camera::{TestCamera, TestCameraBuilder};
use test_hikvision::onvif::notification;

const CHILD_CASE: &str = "KEEPPEEK_NATIVE_EVENTS_CASE";
const CHILD_ROOT: &str = "KEEPPEEK_NATIVE_EVENTS_ROOT";
const CASE_TIMEOUT: Duration = Duration::from_secs(10);
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(5);
const EMPTY_METADATA: &[u8] =
    br#"<tt:MetadataStream xmlns:tt="http://www.onvif.org/ver10/schema"/>"#;

#[test]
fn motion_start_and_clear_persist_one_camera_event() {
    isolated(|root| {
        assert_motion(
            root,
            "vnd.onvif.metadata",
            motion_documents(
                motion(true, "2026-09-05T12:00:00Z"),
                motion(false, "2026-09-05T12:00:02Z"),
            ),
        );
    });
}

#[test]
fn gzip_motion_start_and_clear_persist_one_camera_event() {
    isolated(|root| {
        assert_motion(
            root,
            "vnd.onvif.metadata+gzip",
            motion_documents(GZIP_MOTION_START.to_vec(), GZIP_MOTION_CLEAR.to_vec()),
        );
    });
}

#[test]
fn malformed_and_unsupported_xml_do_not_stop_events_or_video() {
    isolated(|root| {
        let mut documents = vec![
            b"<tt:MetadataStream".to_vec(),
            br#"<MetadataStream xmlns="urn:unsupported"><Motion>true</Motion></MetadataStream>"#
                .to_vec(),
        ];
        documents.extend(motion_documents(
            motion(true, "2026-09-05T12:00:00Z"),
            motion(false, "2026-09-05T12:00:02Z"),
        ));
        assert_motion(root, "vnd.onvif.metadata", documents);
    });
}

#[test]
fn classified_person_box_and_delete_persist_without_image_association() {
    isolated(|root| {
        let person = analytics(
            "2026-09-05T12:00:00Z",
            r#"<tt:Object ObjectId="7"><tt:Appearance>
                <tt:Class><tt:Type Likelihood="0.875">Human</tt:Type></tt:Class>
                <tt:Shape><tt:BoundingBox left="-0.5" right="0.5" top="0.5" bottom="-0.5"/></tt:Shape>
            </tt:Appearance></tt:Object>"#,
        );
        let mut documents = vec![person.clone(), person];
        documents.extend(vec![EMPTY_METADATA.to_vec(); 8]);
        documents.push(analytics(
            "2026-09-05T12:00:02Z",
            r#"<tt:ObjectTree><tt:Delete ObjectId="7"/></tt:ObjectTree>"#,
        ));
        let recorder = Recorder::start(root, Some(("vnd.onvif.metadata", documents)));
        let deletion_deadline = Instant::now() + Duration::from_secs(3);
        let started = recorder.wait_events_until(deletion_deadline, |events| {
            events.len() == 1 && events[0].end_time_ms.is_none()
        });
        assert_eq!(started[0].source, EventSource::Camera);
        assert_eq!(started[0].kind, "person");
        assert_eq!(started[0].revision, 1);
        assert_eq!(started[0].confidence, Some(0.875));
        let payload = started[0].payload.as_ref().unwrap();
        assert_eq!(payload["object_id"], "7");
        assert_eq!(payload["analytics_module"], "source-2");
        assert_eq!(
            payload["object_box"],
            serde_json::json!([0.25, 0.25, 0.5, 0.5])
        );
        assert_unattached(&started[0]);
        let video = recorder.wait_video_after([0, 0]);
        let ended = recorder.wait_events_until(deletion_deadline, |events| {
            events.len() == 1 && events[0].end_time_ms.is_some()
        });
        assert_eq!(ended[0].id, started[0].id);
        assert_eq!(ended[0].source, EventSource::Camera);
        assert_eq!(ended[0].kind, "person");
        assert_eq!(ended[0].confidence, started[0].confidence);
        assert_eq!(ended[0].payload, started[0].payload);
        assert_eq!(ended[0].revision, 2);
        assert!(ended[0].end_time_ms.unwrap() >= ended[0].start_time_ms);
        assert_unattached(&ended[0]);
        recorder.wait_video_after(video);
        assert_eq!(recorder.finish(), ended);
    });
}

#[test]
fn plain_rtsp_records_both_streams_without_creating_events() {
    isolated(|root| {
        let recorder = Recorder::start(root, None);
        let video = recorder.wait_video_after([0, 0]);
        assert!(recorder.events().is_empty());
        recorder.wait_video_after(video);
        assert!(recorder.finish().is_empty());
    });
}

fn assert_motion(root: &Path, encoding: &str, documents: Vec<Vec<u8>>) {
    let recorder = Recorder::start(root, Some((encoding, documents)));
    let started =
        recorder.wait_events(|events| events.len() == 1 && events[0].end_time_ms.is_none());
    assert_eq!(started[0].source, EventSource::Camera);
    assert_eq!(started[0].kind, "motion");
    assert_eq!(started[0].revision, 1);
    assert_eq!(started[0].camera_id, recorder.camera_id);
    let video = recorder.wait_video_after([0, 0]);
    let ended = recorder.wait_events(|events| events.len() == 1 && events[0].end_time_ms.is_some());
    assert_eq!(ended[0].id, started[0].id);
    assert_eq!(ended[0].source, EventSource::Camera);
    assert_eq!(ended[0].kind, "motion");
    assert_eq!(ended[0].start_time_ms, started[0].start_time_ms);
    assert_eq!(ended[0].revision, 2);
    assert!(ended[0].end_time_ms.unwrap() >= ended[0].start_time_ms);
    assert_unattached(&ended[0]);
    assert_eq!(
        recorder.store.event_by_id(&started[0].id).unwrap(),
        Some(ended[0].clone())
    );
    recorder.wait_video_after(video);
    assert_eq!(recorder.finish(), ended);
}

fn assert_unattached(event: &TimelineEvent) {
    assert!(event.bbox.is_none());
    assert!(event.bbox_attachment_id.is_none());
    assert!(event.thumbnail_filename.is_none());
    assert!(event.canonical_attachment_id.is_none());
    assert!(event.attachments.is_empty());
}

fn motion_documents(start: Vec<u8>, clear: Vec<u8>) -> Vec<Vec<u8>> {
    let mut documents = vec![start; 10];
    documents.push(clear);
    documents
}

fn motion(active: bool, timestamp: &str) -> Vec<u8> {
    let message = notification(
        "VideoSource/MotionAlarm",
        active,
        "Changed",
        timestamp,
        "source-2",
    );
    format!(
        r#"<tt:MetadataStream xmlns:tt="http://www.onvif.org/ver10/schema"><tt:Event>{message}</tt:Event></tt:MetadataStream>"#
    )
    .into_bytes()
}

fn analytics(timestamp: &str, contents: &str) -> Vec<u8> {
    format!(
        r#"<tt:MetadataStream xmlns:tt="http://www.onvif.org/ver10/schema">
            <tt:VideoAnalytics><tt:Frame UtcTime="{timestamp}" Source="source-2">
                {contents}
            </tt:Frame></tt:VideoAnalytics>
        </tt:MetadataStream>"#
    )
    .into_bytes()
}

struct Recorder {
    shutdown: Shutdown,
    worker: Option<JoinHandle<()>>,
    store: EventStore,
    camera_id: String,
    storage: Option<StorageEngine>,
    catalog: RecordingCatalog,
    _fake: TestCamera,
}

impl Recorder {
    fn start(root: &Path, metadata: Option<(&str, Vec<Vec<u8>>)>) -> Self {
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/cc-4k-640x360-h264.mp4");
        let mut builder = TestCameraBuilder::rtsp(&source, &source);
        if let Some((encoding, documents)) = metadata {
            builder = builder.metadata(encoding, documents);
        }
        let fake = builder.realtime_start_at(Duration::ZERO).start().unwrap();
        let config_path = root.join("config.toml");
        fs::write(&config_path, fake.connection().toml_entry("metadata")).unwrap();
        let mut configs = load_cameras(&config_path).unwrap();
        let mut config: CameraConfig = configs.remove("test-camera").unwrap().remove(0);
        assert_eq!(config.ip, fake.connection().endpoint_ip());
        config.events.mode = EventMode::RtspMetadata;
        config.events.snapshots = false;
        config.events.source_tokens = vec!["source-2".to_owned()];
        config.record_generic_motion_events = true;
        config.recording_mode = CameraRecordingMode::Both;
        let camera_ip = config.ip;
        let camera = configured_cameras(&HashMap::from([("test-camera".to_owned(), vec![config])]))
            .remove(&camera_ip)
            .unwrap();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let store =
            EventStore::new(catalog.handle(), &root.join("thumbnails"), 1024 * 1024).unwrap();
        let storage = StorageEngine::start_with_catalog(storage_config(root), catalog.handle());
        let shutdown = Shutdown::new();
        let mut recorder = KeepPeekLoop::new(shutdown.clone(), Some(storage.handle()));
        recorder.set_event_store(store.clone());
        recorder.set_health_registry(HealthRegistry::new());
        recorder.add_camera(&camera, true, true).unwrap();
        let worker = thread::spawn(move || recorder.run());
        Self {
            shutdown,
            worker: Some(worker),
            store,
            camera_id: camera_ip.to_string(),
            storage: Some(storage),
            catalog,
            _fake: fake,
        }
    }

    fn events(&self) -> Vec<TimelineEvent> {
        self.store
            .events_in_range(&self.camera_id, 0, i64::MAX)
            .unwrap()
    }

    fn wait_events(&self, ready: impl Fn(&[TimelineEvent]) -> bool) -> Vec<TimelineEvent> {
        self.wait_events_until(Instant::now() + OBSERVATION_TIMEOUT, ready)
    }

    fn wait_events_until(
        &self,
        deadline: Instant,
        ready: impl Fn(&[TimelineEvent]) -> bool,
    ) -> Vec<TimelineEvent> {
        loop {
            let events = self.events();
            assert!(
                Instant::now() < deadline,
                "event observation timed out: {events:?}"
            );
            if ready(&events) {
                return events;
            }
            thread::park_timeout(Duration::from_millis(10));
        }
    }

    fn wait_video_after(&self, previous: [i64; 2]) -> [i64; 2] {
        let deadline = Instant::now() + OBSERVATION_TIMEOUT;
        loop {
            let latest = ["main", "sub"].map(|stream| {
                let fragments = self
                    .catalog
                    .handle()
                    .fragments_in_range(&format!("metadata/{stream}"), 0, i64::MAX)
                    .unwrap();
                fragments.last().map_or(0, |fragment| {
                    assert!(fragment.random_access);
                    assert!(fragment.duration_ms > 0);
                    assert!(fragment.byte_len > 0);
                    fragment.start_ms
                })
            });
            assert!(
                Instant::now() < deadline,
                "video did not progress: {previous:?} -> {latest:?}"
            );
            if latest[0] > previous[0] && latest[1] > previous[1] {
                return latest;
            }
            thread::park_timeout(Duration::from_millis(20));
        }
    }

    fn finish(mut self) -> Vec<TimelineEvent> {
        self.stop();
        for stream in ["main", "sub"] {
            let fragments = self
                .catalog
                .handle()
                .media_fragments_in_range(&format!("metadata/{stream}"), 0, i64::MAX)
                .unwrap();
            let last = fragments.last().expect("recorded video fragment");
            let file = fs::File::open(&last.path).unwrap();
            assert!(file.metadata().unwrap().len() >= last.byte_offset + last.byte_len);
            let recording = mp4::read_mp4(file).unwrap();
            assert!(recording.is_fragmented());
            assert_eq!(recording.tracks().len(), 1);
            assert!(recording.sample_count(1).unwrap() >= 15);
        }
        self.events()
    }

    fn stop(&mut self) {
        self.shutdown.cancel();
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.expect("KeepPeek recorder thread panicked");
            }
        }
        if let Some(storage) = self.storage.take() {
            storage.shutdown();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn storage_config(root: &Path) -> StorageConfig {
    StorageConfig {
        medium_term_path: root.join("recordings"),
        long_term_path: root.join("recordings"),
        recording_catalog_path: root.join("recordings.db"),
        event_thumbnail_path: root.join("thumbnails"),
        event_thumbnail_max_bytes: 1024 * 1024,
        short_term_duration: Duration::ZERO,
        medium_term_duration: Duration::from_secs(30),
        flush_interval: Duration::ZERO,
        write_buffer_bytes: 8 * 1024,
        long_term_max_bytes: 0,
        minimum_free_bytes: 0,
        maximum_used_percent: None,
        warning_free_bytes: 0,
        critical_free_bytes: 0,
        cleanup_hysteresis_bytes: 0,
    }
}

fn isolated(test: impl FnOnce(&Path)) {
    let current = thread::current();
    let name = current.name().expect("integration test has a name");
    if std::env::var(CHILD_CASE).as_deref() == Ok(name) {
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("child fixture root"));
        test(&root);
        fs::write(root.join("completed"), name).unwrap();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(CHILD_CASE, name)
            .env(CHILD_ROOT, root.path())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + CASE_TIMEOUT;
    loop {
        assert!(
            Instant::now() < deadline,
            "native event child exceeded {CASE_TIMEOUT:?}"
        );
        if let Some(exit) = child.0.try_wait().unwrap() {
            assert!(exit.success(), "native event child failed: {exit}");
            assert_eq!(
                fs::read_to_string(root.path().join("completed")).unwrap(),
                name
            );
            return;
        }
        thread::park_timeout(Duration::from_millis(10));
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            self.0
                .kill()
                .expect("terminate timed-out native event child");
            self.0.wait().expect("reap native event child");
        }
    }
}

const GZIP_MOTION_START: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x7d, 0x92, 0xcd, 0x6a, 0xc3, 0x30,
    0x10, 0x84, 0x5f, 0xc5, 0xe8, 0xee, 0xac, 0x63, 0x68, 0xa1, 0xc6, 0x31, 0x94, 0x24, 0x87, 0x1e,
    0x92, 0x16, 0xec, 0xe6, 0xd0, 0x9b, 0x2a, 0x6f, 0x1c, 0x51, 0x4b, 0x32, 0xd2, 0xda, 0x4e, 0xdf,
    0xbe, 0x72, 0x9c, 0x5f, 0xda, 0x06, 0x84, 0x10, 0x68, 0xf6, 0x9b, 0x59, 0xad, 0x52, 0xa2, 0x64,
    0x85, 0xc4, 0x4b, 0x4e, 0x3c, 0x27, 0x8b, 0x5c, 0x05, 0x7b, 0x55, 0x6b, 0x97, 0x10, 0xcd, 0xd8,
    0x8e, 0xa8, 0x49, 0x00, 0xfa, 0xbe, 0x9f, 0x18, 0xdd, 0xc9, 0xed, 0xc4, 0xd8, 0x0a, 0x3a, 0xb4,
    0xd3, 0x08, 0x9c, 0xd8, 0xa1, 0xe2, 0xec, 0x28, 0xee, 0x9d, 0xbe, 0xc8, 0x4b, 0x23, 0xdc, 0xc4,
    0x70, 0x27, 0x5d, 0x68, 0x1a, 0xd4, 0x87, 0x22, 0x2f, 0x80, 0xcf, 0x30, 0x3e, 0xe9, 0x49, 0xbb,
    0xe9, 0x5d, 0x3c, 0x99, 0x46, 0x0a, 0xc7, 0xb2, 0xd4, 0xc7, 0x5b, 0x76, 0xa8, 0x29, 0x4b, 0x07,
    0x8f, 0x64, 0x6d, 0x48, 0x6e, 0xa5, 0xe0, 0x24, 0x8d, 0x5e, 0xa1, 0x73, 0xbc, 0xc2, 0xe3, 0x4d,
    0x31, 0x54, 0x04, 0x0b, 0xc9, 0x6b, 0x14, 0xf7, 0xa3, 0x13, 0x76, 0x23, 0x7f, 0xb9, 0x6f, 0xac,
    0x67, 0x78, 0x14, 0xcc, 0x8d, 0x16, 0x16, 0x09, 0x73, 0x24, 0x96, 0x0d, 0xe9, 0x92, 0x8d, 0x2c,
    0xd1, 0xe4, 0xa6, 0xb5, 0x02, 0x61, 0x65, 0x06, 0xbf, 0xe7, 0x9a, 0x5b, 0x95, 0xc2, 0xc5, 0xed,
    0xe8, 0x7c, 0xce, 0x41, 0xe7, 0x73, 0xf0, 0x4e, 0xa2, 0x90, 0x0a, 0x67, 0x2c, 0x8e, 0xe2, 0xc7,
    0x30, 0x7a, 0x0a, 0xa3, 0x87, 0x62, 0x1a, 0x27, 0x51, 0xe4, 0xd7, 0x07, 0x0b, 0xde, 0xac, 0x7f,
    0x19, 0x4b, 0xdf, 0xaf, 0x7e, 0x3f, 0xf4, 0x32, 0x63, 0xf3, 0x1d, 0xd7, 0x15, 0x96, 0x63, 0xcb,
    0xa3, 0xef, 0x78, 0x94, 0xaa, 0xa9, 0xf1, 0x85, 0x50, 0x05, 0x6b, 0x3e, 0x10, 0xaf, 0x82, 0xf9,
    0xd4, 0x5b, 0x59, 0xb5, 0x23, 0xa2, 0x30, 0x5f, 0xa8, 0x59, 0xb0, 0xe1, 0x75, 0xeb, 0x55, 0xee,
    0x20, 0xf0, 0x2f, 0x0e, 0x59, 0x0a, 0xb7, 0xc4, 0x85, 0x1f, 0xf5, 0xdf, 0xe8, 0x9c, 0x38, 0xe1,
    0x19, 0x41, 0xb6, 0xc5, 0x53, 0xf9, 0x58, 0x03, 0x74, 0xd5, 0x2d, 0xdc, 0x36, 0x0f, 0xff, 0xcf,
    0x07, 0x2e, 0x43, 0x84, 0x5f, 0xdf, 0x2d, 0xfb, 0x01, 0x3f, 0x77, 0x9f, 0x86, 0x82, 0x02, 0x00,
    0x00,
];

const GZIP_MOTION_CLEAR: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x7d, 0x92, 0xcd, 0x6e, 0x83, 0x30,
    0x10, 0x84, 0x5f, 0x05, 0xf9, 0x4e, 0xec, 0x20, 0xb5, 0x52, 0x11, 0x41, 0xaa, 0x92, 0x1c, 0x7a,
    0x48, 0x5a, 0x09, 0x9a, 0x43, 0x6f, 0x5b, 0xb3, 0x10, 0xab, 0x60, 0x23, 0x7b, 0x03, 0xe9, 0xdb,
    0xd7, 0x84, 0xfc, 0xaa, 0x6d, 0x2e, 0x96, 0x25, 0xcf, 0x7e, 0x33, 0xbb, 0xeb, 0x84, 0x28, 0x5e,
    0x21, 0x41, 0x01, 0x04, 0x19, 0x59, 0x84, 0x26, 0xd8, 0x37, 0xb5, 0x76, 0x31, 0xd1, 0x8c, 0x6d,
    0x89, 0xda, 0x98, 0xf3, 0xbe, 0xef, 0x27, 0x46, 0x77, 0xaa, 0x9c, 0x18, 0x5b, 0xf1, 0x0e, 0xed,
    0x54, 0x70, 0x27, 0xb7, 0xd8, 0x00, 0x3b, 0x8a, 0x7b, 0xa7, 0x2f, 0xf2, 0xc2, 0x48, 0x37, 0x31,
    0xe0, 0x94, 0x0b, 0x4d, 0x8b, 0xfa, 0x50, 0xe4, 0x05, 0xfc, 0x33, 0x8c, 0x4e, 0x7a, 0xd2, 0x6e,
    0x7a, 0x17, 0x4f, 0xa6, 0x55, 0xd2, 0xb1, 0x34, 0xf1, 0xf1, 0x96, 0x1d, 0x6a, 0x4a, 0x93, 0xc1,
    0x23, 0x5e, 0x1b, 0x52, 0xa5, 0x92, 0x40, 0xca, 0xe8, 0x15, 0x3a, 0x07, 0x15, 0x1e, 0x5f, 0xf2,
    0xa1, 0x22, 0x58, 0x28, 0xa8, 0x51, 0xde, 0x8f, 0x4e, 0xd8, 0x8d, 0xfc, 0xe5, 0xbe, 0xb5, 0x9e,
    0xe1, 0x51, 0x7c, 0x6e, 0xb4, 0xb4, 0x48, 0x98, 0x21, 0xb1, 0x74, 0x48, 0x17, 0x6f, 0x54, 0x81,
    0x26, 0x33, 0x3b, 0x2b, 0x91, 0xaf, 0xcc, 0xe0, 0xf7, 0x5c, 0x83, 0x6d, 0x12, 0x7e, 0x71, 0x3b,
    0x3a, 0x9f, 0x73, 0xd0, 0xf9, 0x1e, 0xbc, 0x93, 0xcc, 0x55, 0x83, 0x33, 0x16, 0x89, 0xe8, 0x31,
    0x14, 0x4f, 0xa1, 0x78, 0xc8, 0xa7, 0x51, 0x2c, 0x44, 0x2c, 0xa2, 0x0f, 0x16, 0xbc, 0x59, 0x3f,
    0x19, 0x4b, 0xdf, 0xaf, 0xfe, 0x3c, 0xf4, 0x32, 0x63, 0xf3, 0x2d, 0xe8, 0x0a, 0x8b, 0xb1, 0xe5,
    0xd1, 0x77, 0xbc, 0xaa, 0xa6, 0xad, 0xf1, 0x85, 0xb0, 0x09, 0xd6, 0x30, 0x10, 0xaf, 0x82, 0xf9,
    0xd4, 0xa5, 0xaa, 0x76, 0x23, 0x22, 0x37, 0x5f, 0xa8, 0x59, 0xb0, 0x81, 0x7a, 0xe7, 0x55, 0xee,
    0x20, 0xf0, 0x13, 0xe7, 0x69, 0xc2, 0x6f, 0x89, 0x0b, 0xbf, 0xea, 0xbf, 0xd1, 0x19, 0x01, 0xe1,
    0x19, 0x51, 0x42, 0xed, 0xf0, 0x54, 0x3f, 0x16, 0x71, 0xba, 0x6a, 0x97, 0xdf, 0x76, 0xcf, 0xff,
    0x5f, 0x10, 0xbf, 0x6c, 0x91, 0xff, 0xfa, 0x6f, 0xe9, 0x0f, 0x9d, 0xbd, 0x6a, 0x7b, 0x83, 0x02,
    0x00, 0x00,
];
