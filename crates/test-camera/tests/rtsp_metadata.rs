use retina::{
    client::core::{
        ClientOptions, Command, Event, Input, Output, RtspClient, TcpConnectionId, Time,
    },
    codec::{CodecItem, CompressionType, ParametersRef},
};
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};
use test_camera::{TestCameraBuilder, Transport};

const GZIP_DOCUMENT: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xb3, 0xf1, 0x4d, 0x2d, 0x49, 0x4c,
    0x49, 0x2c, 0x49, 0x0c, 0x2e, 0x29, 0x4a, 0x4d, 0xcc, 0xd5, 0xb7, 0x03, 0x00, 0x96, 0x55, 0xc4,
    0xd9, 0x11, 0x00, 0x00, 0x00,
];

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/cc-4k-640x360-h264.mp4")
}

#[test]
fn metadata_rejects_too_many_documents() {
    let source = fixture();
    let error = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", vec![vec![0]; 65])
        .start()
        .err()
        .expect("metadata configuration must reject more than 64 documents");

    assert!(error.to_string().contains("64"), "{error:#}");
}

#[test]
fn metadata_rejects_oversized_document() {
    let source = fixture();
    let error = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", vec![vec![0; 1024 * 1024 + 1]])
        .start()
        .err()
        .expect("oversized document must fail configuration");
    assert!(error.to_string().contains("1 MiB"), "{error:#}");
}

#[test]
fn metadata_rejects_oversized_stream() {
    let source = fixture();
    let error = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", vec![vec![0; 1024 * 1024]; 9])
        .start()
        .err()
        .expect("oversized stream must fail configuration");
    assert!(error.to_string().contains("8 MiB"), "{error:#}");
}

#[test]
fn metadata_accepts_exact_byte_limits() {
    let source = fixture();
    let camera = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", vec![vec![0; 1024 * 1024]; 8])
        .start()
        .unwrap();
    assert_ne!(camera.connection().onvif_port(), 0);
}

#[test]
fn metadata_accepts_exact_document_count_limit() {
    let source = fixture();
    let camera = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", vec![b"<metadata/>".to_vec(); 64])
        .start()
        .unwrap();
    assert_ne!(camera.connection().onvif_port(), 0);
}

#[test]
fn metadata_rejects_invalid_encoding_tokens() {
    let source = fixture();
    for encoding in [
        "".to_owned(),
        "vnd.onvif.metadata\r\nm=video 0 RTP/AVP 96".to_owned(),
        "x".repeat(129),
    ] {
        let error = TestCameraBuilder::rtsp(&source, &source)
            .metadata(&encoding, Vec::new())
            .start()
            .err()
            .expect("invalid SDP encoding token must fail configuration");
        assert!(error.to_string().contains("encoding"), "{error:#}");
    }
}

#[test]
fn metadata_rejects_reo_proto() {
    let source = fixture();
    let error = TestCameraBuilder::reo_proto(&source, &source)
        .metadata("vnd.onvif.metadata", Vec::new())
        .start()
        .err()
        .expect("Reolink metadata must fail configuration");
    assert!(error.to_string().contains("RTSP"), "{error:#}");
}

#[test]
fn metadata_rejects_udp_configuration() {
    let source = fixture();
    let error = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", Vec::new())
        .transport(Transport::Udp)
        .start()
        .err()
        .expect("unsupported metadata transport must fail configuration");
    assert!(error.to_string().contains("TCP"), "{error:#}");
}

#[test]
fn metadata_public_client_receives_documents_and_video() {
    let source = fixture();
    let documents = vec![
        format!("<MetadataStream>{}</MetadataStream>", "x".repeat(4000)).into_bytes(),
        b"<malformed XML".to_vec(),
    ];
    let camera = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", documents.clone())
        .start()
        .unwrap();

    for url in [
        camera.connection().main_stream_url(),
        camera.connection().sub_stream_url(),
    ] {
        let mut client = Client::connect(url);
        client.receive_until(|client| client.documents.len() == 2 && client.video_frames > 0);
        assert_eq!(client.documents[0].1, documents[0]);
        assert_eq!(client.documents[1].1, documents[1]);
        assert!(client.documents[1].0 > client.documents[0].0);
        let streams = client.core.streams().unwrap();
        assert_eq!(streams[1].media(), "application");
        assert_eq!(streams[1].encoding_name(), "vnd.onvif.metadata");
        assert_eq!(streams[1].rtp_payload_type(), 107);
        assert_eq!(streams[1].clock_rate_hz(), 90_000);
        assert_ne!(streams[0].control(), streams[1].control());
    }

    let connection = camera.connection();
    let config = connection.toml_entry("metadata");
    assert_ne!(connection.onvif_port(), 0);
    assert!(config.contains(&format!("onvif_port = {}", connection.onvif_port())));
    assert!(config.contains(connection.main_stream_url()));
    assert!(config.contains(connection.sub_stream_url()));
}

#[test]
fn metadata_encodings_preserve_raw_bytes_with_mixed_video() {
    let h264 = fixture();
    let h265 = h264.with_file_name("cc-4k-640x360-h265.mp4");
    for (encoding, compression, document) in [
        (
            "vnd.onvif.metadata+gzip",
            CompressionType::GzipCompressed,
            GZIP_DOCUMENT,
        ),
        (
            "vnd.onvif.metadata.gzip",
            CompressionType::GzipCompressed,
            GZIP_DOCUMENT,
        ),
        (
            "vnd.onvif.metadata.exi.onvif",
            CompressionType::ExiDefault,
            &[0xa0, 0x01][..],
        ),
        (
            "vnd.onvif.metadata.exi.ext",
            CompressionType::ExiInBand,
            &[0xa0, 0x02][..],
        ),
    ] {
        let camera = TestCameraBuilder::rtsp(&h265, &h264)
            .metadata(encoding, vec![document.to_vec()])
            .start()
            .unwrap();
        for (url, video_encoding) in [
            (camera.connection().main_stream_url(), "h265"),
            (camera.connection().sub_stream_url(), "h264"),
        ] {
            let mut client = Client::connect(url);
            client.receive_until(|client| client.documents.len() == 1 && client.video_frames > 0);
            assert_eq!(client.documents[0].1, document, "{encoding}");
            let streams = client.core.streams().unwrap();
            assert_eq!(streams[0].encoding_name(), video_encoding);
            assert_eq!(streams[1].encoding_name(), encoding);
            let Some(ParametersRef::Message(parameters)) = streams[1].parameters() else {
                panic!("metadata parameters are missing for {encoding}");
            };
            assert_eq!(parameters.compression_type(), compression);
        }
    }
}

#[test]
fn metadata_is_not_repeated_by_video_loops_and_profiles_can_run_together() {
    let source = fixture();
    let documents = vec![b"<first/>".to_vec(), b"<second/>".to_vec()];
    let camera = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", documents.clone())
        .realtime_start_at(Duration::ZERO)
        .start()
        .unwrap();
    let mut main = Client::connect(camera.connection().main_stream_url());
    let mut sub = Client::connect(camera.connection().sub_stream_url());
    main.receive_until(|client| client.documents.len() == 2 && client.video_frames > 0);
    sub.receive_until(|client| client.documents.len() == 2 && client.video_frames > 0);
    main.receive_until(|client| client.video_frames >= 32);
    sub.receive_until(|client| client.video_frames >= 32);
    assert_eq!(main.documents, sub.documents);
    assert_eq!(
        main.documents,
        vec![(0, documents[0].clone()), (9000, documents[1].clone())]
    );
}

#[test]
fn metadata_documents_restart_with_stable_timestamps_on_new_play_session() {
    let source = fixture();
    let camera = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", vec![b"<metadata/>".to_vec()])
        .start()
        .unwrap();
    for _ in 0..2 {
        let mut client = Client::connect(camera.connection().main_stream_url());
        client.receive_until(|client| client.documents.len() == 1 && client.video_frames > 0);
        assert_eq!(client.documents, vec![(0, b"<metadata/>".to_vec())]);
    }
}

#[test]
fn metadata_empty_list_keeps_video_running() {
    let source = fixture();
    let camera = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", Vec::new())
        .start()
        .unwrap();
    let mut client = Client::connect(camera.connection().main_stream_url());
    client.receive_until(|client| client.video_frames >= 10);
    assert_eq!(client.core.streams().unwrap().len(), 2);
    assert!(client.documents.is_empty());
}

#[test]
fn metadata_default_keeps_video_only_presentation() {
    let source = fixture();
    let camera = TestCameraBuilder::rtsp(&source, &source).start().unwrap();
    let mut client = Client::connect(camera.connection().main_stream_url());
    client.receive_until(|client| client.video_frames >= 10);
    assert_eq!(client.core.streams().unwrap().len(), 1);
    assert!(client.documents.is_empty());
}

#[test]
fn metadata_oversize_for_retina_does_not_stop_later_documents_or_video() {
    let source = fixture();
    let camera = TestCameraBuilder::rtsp(&source, &source)
        .metadata(
            "vnd.onvif.metadata",
            vec![vec![b'x'; 256 * 1024 + 1], b"<recovered/>".to_vec()],
        )
        .realtime_start_at(Duration::ZERO)
        .start()
        .unwrap();
    let mut client = Client::connect(camera.connection().main_stream_url());
    client.receive_until(|client| !client.documents.is_empty() && client.video_frames >= 16);
    assert_eq!(client.documents, vec![(9000, b"<recovered/>".to_vec())]);
}

struct Client {
    core: RtspClient,
    socket: Option<(TcpConnectionId, TcpStream)>,
    video_frames: usize,
    documents: Vec<(i64, Vec<u8>)>,
}

impl Client {
    fn connect(url: &str) -> Self {
        let mut client = Self {
            core: RtspClient::new(ClientOptions {
                response_timeout: Duration::from_secs(3),
                credentials: None,
            }),
            socket: None,
            video_frames: 0,
            documents: Vec::new(),
        };
        client.command(Command::Describe {
            url: url.parse().unwrap(),
        });
        client
    }

    fn command(&mut self, command: Command) {
        self.core
            .handle_input(Input::Command {
                time: now(),
                command,
            })
            .unwrap();
    }

    fn receive_until(&mut self, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buffer = [0; 16 * 1024];
        loop {
            self.drain();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for metadata and video"
            );
            let (connection, stream) = self.socket.as_mut().unwrap();
            match stream.read(&mut buffer) {
                Ok(0) => panic!("RTSP connection closed before receiving the expected media"),
                Ok(length) => self
                    .core
                    .handle_input(Input::TcpData {
                        time: now(),
                        connection: *connection,
                        data: &buffer[..length],
                    })
                    .unwrap(),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    self.core
                        .handle_input(Input::Timeout { time: now() })
                        .unwrap();
                }
                Err(error) => panic!("RTSP read failed: {error}"),
            }
        }
    }

    fn drain(&mut self) {
        for _ in 0..4096 {
            match self.core.poll_output() {
                Output::OpenTcp { connection, target } => {
                    let stream = TcpStream::connect((target.host.as_ref(), target.port)).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_millis(50)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    self.socket = Some((connection, stream));
                    self.core
                        .handle_input(Input::TcpConnected {
                            time: now(),
                            connection,
                        })
                        .unwrap();
                }
                Output::TcpTransmit { connection, data } => {
                    self.socket.as_mut().unwrap().1.write_all(&data).unwrap();
                    self.core
                        .handle_input(Input::TcpWriteCompleted {
                            time: now(),
                            connection,
                        })
                        .unwrap();
                }
                Output::Event(event) => self.event(event),
                Output::Timeout(_) => return,
                output => panic!("unexpected Retina output: {output:?}"),
            }
        }
        panic!("Retina output failed to quiesce");
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::DescribeResponse { .. } => self.command(Command::Setup { stream: 0 }),
            Event::SetupResponse { stream, .. } => {
                if stream + 1 < self.core.streams().unwrap().len() {
                    self.command(Command::Setup { stream: stream + 1 });
                } else {
                    self.command(Command::Play);
                }
            }
            Event::PlayResponse { response, .. } => assert!(response.status_code.is_success()),
            Event::CodecItem(CodecItem::VideoFrame(frame)) => {
                assert!(!frame.data().is_empty());
                self.video_frames += 1;
            }
            Event::CodecItem(CodecItem::MessageFrame(frame)) => {
                assert_eq!(frame.stream_id(), 1);
                self.documents
                    .push((frame.timestamp().timestamp(), frame.data().to_vec()));
            }
            Event::TcpConnected { .. } => {}
            event => panic!("unexpected Retina event: {event:?}"),
        }
    }
}

fn now() -> Time {
    Time {
        monotonic: Instant::now(),
        wall: SystemTime::now(),
    }
}
