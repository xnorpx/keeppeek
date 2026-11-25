use super::{BlockingRtsp, RtspTransport, StreamKind, setup_described_tracks};
use crate::{
    rtsp::audio::ObservedAudio, storage::AudioCodec, webrtc::audio::AudioCodec as LiveCodec,
};
use retina::client::Credentials;
use std::{
    io::{BufRead, BufReader, Write},
    net::{IpAddr, Ipv4Addr, TcpListener},
    time::Duration,
};

#[test]
fn manual_rtsp_audio_uses_the_negotiated_track_clock_layout_and_decoder_configuration() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("rtsp://{}/camera", listener.local_addr().unwrap());
    let responder = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut lines = BufReader::new(&socket).lines().map(Result::unwrap);
        assert!(lines.next().unwrap().starts_with("DESCRIBE "));
        let seq = lines
            .take_while(|line| !line.is_empty())
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("cseq"))
                    .map(|(_, value)| value.trim().to_owned())
            })
            .unwrap();
        let sdp = concat!(
            "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=calibration\r\nt=0 0\r\n",
            "m=audio 0 RTP/AVP 96\r\na=control:track1\r\na=rtpmap:96 MPEG4-GENERIC/48000/2\r\n",
            "a=fmtp:96 streamtype=5;profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;config=1190\r\n",
            "m=audio 0 RTP/AVP 8\r\na=control:track2\r\n",
            "m=audio 0 RTP/AVP 0\r\na=control:track3\r\n",
            "m=audio 0 RTP/AVP 97\r\na=control:track4\r\na=rtpmap:97 L16/48000/2\r\n",
        );
        write!(socket,"RTSP/1.0 200 OK\r\nCSeq: {seq}\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{sdp}",sdp.len()).unwrap();
    });
    let client = BlockingRtsp::describe(
        url.parse().unwrap(),
        Credentials {
            username: String::new(),
            password: String::new(),
        },
    )
    .unwrap();
    responder.join().unwrap();
    let tracks = client.streams();
    let aac = ObservedAudio::from_track(&tracks[0]).unwrap();
    assert_eq!(
        (aac.storage_codec, aac.live_codec),
        (AudioCodec::Aac, LiveCodec::Aac)
    );
    assert_eq!((aac.sample_rate, aac.channels), (48_000, 2));
    assert_eq!(aac.decoder_config, [0x11, 0x90]);
    for (track, storage, live) in [
        (&tracks[1], AudioCodec::G711Alaw, LiveCodec::G711Alaw),
        (&tracks[2], AudioCodec::G711Ulaw, LiveCodec::G711Ulaw),
    ] {
        let audio = ObservedAudio::from_track(track).unwrap();
        assert_eq!((audio.storage_codec, audio.live_codec), (storage, live));
        assert_eq!((audio.sample_rate, audio.channels), (8_000, 1));
        assert!(audio.decoder_config.is_empty());
    }
    assert!(ObservedAudio::from_track(&tracks[3]).is_none());
}

#[test]
fn setup_selects_one_supported_audio_track_in_sdp_order() {
    let cases = [
        (
            concat!(
                "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=multi-audio-aac-first\r\nt=0 0\r\n",
                "m=video 0 RTP/AVP 96\r\na=control:track0\r\na=rtpmap:96 H264/90000\r\n",
                "a=fmtp:96 packetization-mode=1;sprop-parameter-sets=Z0IAKeKQCgC3YC3AQEB4eJEV,aM48gA==\r\n",
                "m=audio 0 RTP/AVP 98\r\na=control:track1\r\na=rtpmap:98 L16/48000/2\r\n",
                "m=audio 0 RTP/AVP 97\r\na=control:track2\r\na=rtpmap:97 MPEG4-GENERIC/48000/2\r\n",
                "a=fmtp:97 streamtype=5;profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;config=1190\r\n",
                "m=audio 0 RTP/AVP 8\r\na=control:track3\r\n",
            ),
            Some(2),
            vec!["track0", "track2"],
        ),
        (
            concat!(
                "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=multi-audio-pcma-first\r\nt=0 0\r\n",
                "m=video 0 RTP/AVP 96\r\na=control:track0\r\na=rtpmap:96 H264/90000\r\n",
                "a=fmtp:96 packetization-mode=1;sprop-parameter-sets=Z0IAKeKQCgC3YC3AQEB4eJEV,aM48gA==\r\n",
                "m=audio 0 RTP/AVP 8\r\na=control:track1\r\n",
                "m=audio 0 RTP/AVP 97\r\na=control:track2\r\na=rtpmap:97 MPEG4-GENERIC/48000/2\r\n",
                "a=fmtp:97 streamtype=5;profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;config=1190\r\n",
            ),
            Some(1),
            vec!["track0", "track1"],
        ),
    ];

    for (sdp, expected_track, expected_setup_paths) in cases {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("rtsp://{}/camera", listener.local_addr().unwrap());
        let responder = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut setup_paths = Vec::new();
            loop {
                let mut request = String::new();
                match reader.read_line(&mut request) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let mut seq = String::new();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("cseq")
                    {
                        seq = value.trim().to_owned();
                    }
                }
                if request.starts_with("DESCRIBE ") {
                    write!(
                        socket,
                        "RTSP/1.0 200 OK\r\nCSeq: {seq}\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{sdp}",
                        sdp.len()
                    )
                    .unwrap();
                } else if request.starts_with("SETUP ") {
                    let target = request.split_whitespace().nth(1).unwrap();
                    let track = target.rsplit('/').next().unwrap().to_owned();
                    setup_paths.push(track);
                    let channel = setup_paths.len() * 2 - 2;
                    write!(
                        socket,
                        "RTSP/1.0 200 OK\r\nCSeq: {seq}\r\nSession: 123456\r\nTransport: RTP/AVP/TCP;unicast;interleaved={channel}-{}\r\n\r\n",
                        channel + 1
                    )
                    .unwrap();
                } else {
                    break;
                }
            }
            setup_paths
        });
        let mut client = BlockingRtsp::describe(
            url.parse().unwrap(),
            Credentials {
                username: String::new(),
                password: String::new(),
            },
        )
        .unwrap();
        let selected = setup_described_tracks(
            &mut client,
            RtspTransport::Tcp,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            StreamKind::Main,
        )
        .unwrap();
        drop(client);
        let setup_paths = responder.join().unwrap();
        assert_eq!(selected, expected_track);
        assert_eq!(setup_paths, expected_setup_paths);
    }
}
