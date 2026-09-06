use retina::{
    client::core::{FramedMessage, RtspFramer},
    rtsp::msg,
};
use rouille::url::Url;
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    time::{Duration, Instant},
};
use test_camera::TestCameraBuilder;

#[test]
fn metadata_wire_respects_channels_fragment_limits_and_document_boundaries() {
    let source = fixture();
    let documents = vec![
        vec![b'a'; 1199],
        vec![b'b'; 1200],
        Vec::new(),
        vec![b'c'; 2400],
    ];
    let camera = TestCameraBuilder::rtsp(&source, &source)
        .metadata("vnd.onvif.metadata", documents.clone())
        .start()
        .unwrap();
    let mut wire = Wire::connect(camera.connection().main_stream_url());
    wire.play();
    let mut sequence = 1;
    for (index, document) in documents.iter().enumerate() {
        let mut received = Vec::new();
        let mut fragments = 0;
        loop {
            let message = wire.next();
            let msg::Message::Data(data) = message.message else {
                panic!("expected RTP data");
            };
            if data.channel_id == 4 {
                continue;
            }
            assert_eq!(data.channel_id, 8);
            let packet = message.body;
            assert_eq!(packet[0], 0x80);
            assert_eq!(packet[1] & 0x7f, 107);
            assert!(packet.len() >= 12);
            assert!(packet.len() - 12 < 1200);
            assert_eq!(
                u16::from_be_bytes(packet[2..4].try_into().unwrap()),
                sequence
            );
            assert_eq!(
                u32::from_be_bytes(packet[4..8].try_into().unwrap()),
                u32::try_from(index).unwrap() * 9000
            );
            received.extend_from_slice(&packet[12..]);
            sequence += 1;
            fragments += 1;
            if packet[1] & 0x80 != 0 {
                break;
            }
            assert!(
                received.len() < document.len(),
                "final fragment lacks a marker"
            );
            assert!(fragments < 8, "too many metadata fragments");
        }
        assert_eq!(&received, document);
        assert_eq!(fragments, [1, 2, 1, 3][index]);
    }
}

#[test]
fn metadata_drop_closes_idle_and_playing_connections() {
    let source = fixture();
    for playing in [false, true] {
        let camera = TestCameraBuilder::rtsp(&source, &source)
            .metadata("vnd.onvif.metadata", vec![vec![b'x'; 1024 * 1024]; 8])
            .realtime_start_at(Duration::ZERO)
            .start()
            .unwrap();
        let mut wire = Wire::connect(camera.connection().main_stream_url());
        if playing {
            wire.play();
        }
        let address = wire.stream.peer_addr().unwrap();
        let onvif = SocketAddr::from((
            camera.connection().endpoint_ip(),
            camera.connection().onvif_port(),
        ));
        let started = Instant::now();
        drop(camera);
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "fixture cleanup exceeded its deadline"
        );
        assert!(TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err());
        assert!(TcpStream::connect_timeout(&onvif, Duration::from_millis(100)).is_err());
        assert_closed(&mut wire.stream);
    }
}

#[test]
fn closure_observation_retries_transient_socket_timeouts() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(25)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    let worker = std::thread::spawn(move || {
        std::thread::park_timeout(Duration::from_millis(150));
        server.shutdown(std::net::Shutdown::Both).unwrap();
    });
    assert_closed(&mut client);
    worker.join().unwrap();
}

#[test]
#[should_panic(expected = "connection remained open after fixture drop")]
fn closure_observation_rejects_a_connection_that_stays_open() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(25)))
        .unwrap();
    let (_server, _) = listener.accept().unwrap();
    assert_closed(&mut client);
}

fn assert_closed(stream: &mut TcpStream) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut buffer = [0; 16 * 1024];
    for _ in 0..1024 {
        assert!(
            Instant::now() < deadline,
            "connection remained open after fixture drop"
        );
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) =>
            {
                return;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("fixture connection did not close: {error}"),
        }
    }
    panic!("fixture emitted more than 16 MiB after drop");
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/cc-4k-640x360-h264.mp4")
}

struct Wire {
    stream: TcpStream,
    url: String,
    session: String,
    cseq: u32,
    framer: RtspFramer,
    pending: VecDeque<FramedMessage>,
}

impl Wire {
    fn connect(url: &str) -> Self {
        let parsed = Url::parse(url).unwrap();
        let stream =
            TcpStream::connect((parsed.host_str().unwrap(), parsed.port().unwrap())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        Self {
            stream,
            url: url.to_owned(),
            session: String::new(),
            cseq: 1,
            framer: RtspFramer::default(),
            pending: VecDeque::new(),
        }
    }

    fn play(&mut self) {
        let describe = self.request("DESCRIBE", "", "Accept: application/sdp\r\n");
        let sdp = std::str::from_utf8(&describe.body).unwrap();
        assert!(sdp.contains("m=application 0 RTP/AVP 107\r\n"));
        assert!(sdp.contains("a=rtpmap:107 vnd.onvif.metadata/90000\r\n"));
        self.request(
            "SETUP",
            "/trackID=1",
            "Transport: RTP/AVP/TCP;unicast;interleaved=8-9\r\n",
        );
        self.request(
            "SETUP",
            "/trackID=0",
            "Transport: RTP/AVP/TCP;unicast;interleaved=4-5\r\n",
        );
        let play = self.request("PLAY", "", "");
        let msg::Message::Response(response) = play.message else {
            panic!("expected PLAY response");
        };
        let info = response.headers.get("RTP-Info").unwrap();
        assert!(info.contains(&format!("url={}/trackID=1;seq=1;rtptime=0", self.url)));
        assert!(info.contains("trackID=0"));
    }

    fn request(&mut self, method: &str, suffix: &str, headers: &str) -> FramedMessage {
        let request = format!(
            "{method} {}{suffix} RTSP/1.0\r\nCSeq: {}\r\n{headers}{}\r\n",
            self.url, self.cseq, self.session
        );
        self.stream.write_all(request.as_bytes()).unwrap();
        let message = self.next();
        let msg::Message::Response(response) = &message.message else {
            panic!("expected {method} response");
        };
        assert_eq!(response.status_code, msg::StatusCode::OK);
        assert_eq!(
            response.headers.get("CSeq").unwrap().to_string(),
            self.cseq.to_string()
        );
        self.cseq += 1;
        if let Some(session) = response.headers.get("Session") {
            self.session = format!("Session: {session}\r\n");
        }
        message
    }

    fn next(&mut self) -> FramedMessage {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut buffer = [0; 16 * 1024];
        loop {
            if let Some(message) = self.pending.pop_front() {
                return message;
            }
            assert!(Instant::now() < deadline, "RTSP wire read timed out");
            match self.stream.read(&mut buffer) {
                Ok(0) => panic!("RTSP wire closed unexpectedly"),
                Ok(length) => self
                    .pending
                    .extend(self.framer.push(&buffer[..length]).unwrap()),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => panic!("RTSP wire read failed: {error}"),
            }
        }
    }
}
