use super::BlockingRtsp;
use crate::{
    rtsp::audio::ObservedAudio, storage::AudioCodec, webrtc::audio::AudioCodec as LiveCodec,
};
use retina::client::Credentials;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
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
