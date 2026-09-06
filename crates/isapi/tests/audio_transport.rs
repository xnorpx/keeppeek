#![cfg(feature = "ureq")]

use std::time::Duration;

use isapi::{Credentials, blocking::Client, management::AudioCodec};
use test_hikvision::FakeHikvision;

fn client(camera: &FakeHikvision) -> Client {
    Client::new(camera.origin(), Credentials::new("test", "test")).unwrap()
}

#[test]
fn audio_session_owns_open_close_and_never_resets_another_owner() {
    let camera = FakeHikvision::builder().start().unwrap();
    let talk = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap();
    assert_eq!(talk.channel().id(), 1);
    assert_eq!(talk.channel().output_codec(), &AudioCodec::G711Ulaw);
    assert!(camera.audio_active());
    let error = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap_err();
    assert_eq!(error.device_status(), Some(2));
    assert!(camera.audio_active());
    assert!(
        camera
            .requests()
            .iter()
            .all(|request| !request.target().ends_with("/close"))
    );
    assert!(talk.close().unwrap().success());
    assert!(!camera.audio_active());
    let talk = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap();
    drop(talk);
    assert!(!camera.audio_active());
}

#[test]
fn audio_rejects_invalid_lifetimes_and_unsupported_channels_before_opening() {
    let camera = FakeHikvision::builder().start().unwrap();
    for lifetime in [Duration::ZERO, Duration::from_secs(301)] {
        assert!(
            client(&camera)
                .open_audio(1, lifetime)
                .unwrap_err()
                .is_invalid_input()
        );
    }
    assert!(camera.requests().is_empty());
    camera.set_resource("/ISAPI/System/TwoWayAudio/channels/1", "<TwoWayAudioChannel><id>1</id><enabled>true</enabled><audioCompressionType>newCodec</audioCompressionType></TwoWayAudioChannel>").unwrap();
    assert!(
        client(&camera)
            .open_audio(1, Duration::from_secs(2))
            .is_err()
    );
    assert!(!camera.audio_active());
    assert!(
        camera
            .requests()
            .iter()
            .all(|request| request.method() == "GET")
    );
}

#[test]
fn speaker_upload_sends_raw_g711_after_one_authenticated_empty_http_handshake() {
    let camera = FakeHikvision::builder().start().unwrap();
    let mut talk = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap();
    let mut speaker = talk.speaker().unwrap();
    let first = [0xff; 160];
    let second = [0x7f; 160];
    assert_eq!(speaker.codec(), &AudioCodec::G711Ulaw);
    speaker.send(&first).unwrap();
    speaker.send(&second).unwrap();
    drop(speaker);
    assert!(camera.wait_for_audio_bytes(320, Duration::from_secs(1)));
    talk.close().unwrap();
    assert_eq!(camera.audio_output(), [first, second].concat());
    let requests = camera.requests();
    let uploads = requests
        .iter()
        .filter(|request| request.target().contains("/audioData"))
        .collect::<Vec<_>>();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].method(), "PUT");
    assert!(uploads[0].authenticated());
    assert_eq!(uploads[0].header("content-length"), Some("0"));
    assert_eq!(
        uploads[0].header("content-type"),
        Some("application/octet-stream")
    );
    assert!(uploads[0].header("transfer-encoding").is_none());
    assert!(uploads[0].body().is_empty());
    assert!(uploads[0].target().ends_with("?sessionId=1"));
}

#[test]
fn closing_audio_invalidates_previously_returned_speaker_handles() {
    let camera = FakeHikvision::builder().start().unwrap();
    let mut talk = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap();
    let mut speaker = talk.speaker().unwrap();
    assert!(talk.speaker().is_err());
    talk.close().unwrap();
    assert!(speaker.send(&[0xff; 160]).unwrap_err().is_cancelled());
    assert!(camera.audio_output().is_empty());
}

#[test]
fn audio_sends_and_receives_concurrently_with_camera_perspective_codecs() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let camera = FakeHikvision::builder().start().unwrap();
    camera.set_resource("/ISAPI/System/TwoWayAudio/channels/1", "<TwoWayAudioChannel><id>1</id><enabled>true</enabled><audioCompressionType>G.711ulaw</audioCompressionType><audioInboundCompressionType>G.711alaw</audioInboundCompressionType></TwoWayAudioChannel>").unwrap();
    camera
        .set_audio_input_after_output(&[0xd5; 320], 160)
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let armed = Arc::new(AtomicBool::new(false));
    let check_armed = Arc::clone(&armed);
    let client = Client::builder(camera.origin(), Credentials::new("test", "test"))
        .cancelled(move || {
            if check_armed.load(Ordering::Acquire) {
                let _ = entered_tx.try_send(());
            }
            false
        })
        .build()
        .unwrap();
    let mut talk = client.open_audio(1, Duration::from_secs(2)).unwrap();
    let mut speaker = talk.speaker().unwrap();
    let mut microphone = talk.microphone().unwrap();
    assert_eq!(speaker.codec(), &AudioCodec::G711Ulaw);
    assert_eq!(microphone.codec(), &AudioCodec::G711Alaw);
    armed.store(true, Ordering::Release);
    std::thread::scope(|scope| {
        let receive = scope.spawn(move || {
            let mut samples = Vec::new();
            let mut buffer = [0; 160];
            while samples.len() < 320 {
                let count = microphone.read(&mut buffer).unwrap();
                assert!(count > 0);
                samples.extend_from_slice(&buffer[..count]);
            }
            samples
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        speaker.send(&[0xff; 160]).unwrap();
        speaker.send(&[0x7f; 160]).unwrap();
        assert_eq!(receive.join().unwrap(), vec![0xd5; 320]);
    });
    assert!(camera.wait_for_audio_bytes(320, Duration::from_secs(1)));
    talk.close().unwrap();
    assert_eq!(camera.audio_output().len(), 320);
    assert!(
        camera
            .requests()
            .iter()
            .any(|request| request.method() == "GET"
                && request.target().ends_with("/audioData?sessionId=1")
                && request.authenticated())
    );
}

#[test]
fn blocked_microphone_receive_is_cancelled_and_cleanup_still_closes_the_camera() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Instant;

    let camera = FakeHikvision::builder().start().unwrap();
    camera.set_audio_input(&[]).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let armed = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let check_cancelled = Arc::clone(&cancelled);
    let check_armed = Arc::clone(&armed);
    let client = Client::builder(camera.origin(), Credentials::new("test", "test"))
        .cancelled(move || {
            if check_armed.load(Ordering::Acquire) {
                let _ = entered_tx.try_send(());
            }
            check_cancelled.load(Ordering::Acquire)
        })
        .build()
        .unwrap();
    let mut talk = client.open_audio(1, Duration::from_secs(3)).unwrap();
    let mut microphone = talk.microphone().unwrap();
    armed.store(true, Ordering::Release);
    let receive = std::thread::spawn(move || microphone.read(&mut [0; 160]));
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let started = Instant::now();
    cancelled.store(true, Ordering::Release);
    assert!(receive.join().unwrap().unwrap_err().is_cancelled());
    assert!(started.elapsed() < Duration::from_secs(1));
    talk.close().unwrap();
    assert!(!camera.audio_active());
}

#[test]
fn speaker_handshake_rejects_ambiguous_media_and_device_errors() {
    for headers in ["", "Content-Type: text/plain\r\n"] {
        let camera = FakeHikvision::builder().start().unwrap();
        let mut talk = client(&camera)
            .open_audio(1, Duration::from_secs(2))
            .unwrap();
        camera.enqueue(test_hikvision::Reply::raw(format!("HTTP/1.1 200 OK\r\n{headers}\r\n<ResponseStatus><statusCode>4</statusCode></ResponseStatus>").into_bytes())).unwrap();
        assert!(talk.speaker().is_err());
        assert!(camera.audio_output().is_empty());
        talk.close().unwrap();
    }
}

#[test]
fn abandoning_lost_audio_ownership_never_closes_a_replacement_session() {
    use isapi::management::AudioChannel;

    let camera = FakeHikvision::builder().start().unwrap();
    let mut old = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap();
    let mut speaker = old.speaker().unwrap();
    let mut outside = client(&camera);
    let channel = outside.query(&AudioChannel::query(1).unwrap()).unwrap();
    outside.command(&channel.close().unwrap()).unwrap();
    let replacement = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap();
    old.abandon();
    assert!(speaker.send(&[0xff; 160]).unwrap_err().is_cancelled());
    assert!(camera.audio_active());
    replacement.close().unwrap();
}

#[test]
fn microphone_rejects_wrong_codecs_and_reports_truncated_bodies() {
    let camera = FakeHikvision::builder().start().unwrap();
    let mut talk = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap();
    camera
        .enqueue(test_hikvision::Reply::http(200, "audio/pcma", [0xd5; 160]))
        .unwrap();
    assert!(talk.microphone().unwrap_err().is_protocol());
    talk.close().unwrap();
    let mut talk = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap();
    camera.enqueue(test_hikvision::Reply::raw(b"HTTP/1.1 200 OK\r\nContent-Type: audio/basic\r\nContent-Length: 160\r\nConnection: close\r\n\r\n1234".to_vec())).unwrap();
    let mut microphone = talk.microphone().unwrap();
    let mut buffer = [0; 160];
    assert_eq!(microphone.read(&mut buffer).unwrap(), 4);
    assert!(microphone.read(&mut buffer).is_err());
    talk.close().unwrap();
}

#[test]
fn audio_lifetime_expires_blocked_reads_and_frame_limits_stop_writes() {
    let camera = FakeHikvision::builder().start().unwrap();
    camera.set_audio_input(&[]).unwrap();
    let mut talk = client(&camera)
        .open_audio(1, Duration::from_millis(300))
        .unwrap();
    let mut microphone = talk.microphone().unwrap();
    let started = std::time::Instant::now();
    assert!(microphone.read(&mut [0; 160]).unwrap_err().is_timeout());
    assert!(started.elapsed() < Duration::from_secs(1));
    talk.close().unwrap();
    let mut talk = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap();
    let mut speaker = talk.speaker().unwrap();
    assert!(speaker.send(&[0xff; 1601]).unwrap_err().is_limit());
    assert!(speaker.send(&[0xff; 160]).is_err());
    assert!(camera.audio_output().is_empty());
    talk.close().unwrap();
}

#[test]
fn microphone_rejects_unsupported_transfer_codings_before_reading_samples() {
    for coding in ["gzip, chunked", "gzip", "chunked, chunked"] {
        let camera = FakeHikvision::builder().start().unwrap();
        let mut talk = client(&camera)
            .open_audio(1, Duration::from_secs(2))
            .unwrap();
        camera.enqueue(test_hikvision::Reply::raw(format!("HTTP/1.1 200 OK\r\nContent-Type: audio/basic\r\nTransfer-Encoding: {coding}\r\nConnection: close\r\n\r\n4\r\n1234\r\n0\r\n\r\n").into_bytes())).unwrap();
        assert!(talk.microphone().is_err());
        talk.close().unwrap();
    }
    let camera = FakeHikvision::builder().start().unwrap();
    let mut talk = client(&camera)
        .open_audio(1, Duration::from_secs(2))
        .unwrap();
    camera.enqueue(test_hikvision::Reply::raw(b"HTTP/1.1 200 OK\r\nContent-Type: audio/basic\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\n1234\r\n0\r\n\r\n".to_vec())).unwrap();
    let mut microphone = talk.microphone().unwrap();
    let mut samples = [0; 4];
    assert_eq!(microphone.read(&mut samples).unwrap(), 4);
    assert_eq!(&samples, b"1234");
    assert_eq!(microphone.read(&mut samples).unwrap(), 0);
    talk.close().unwrap();
}
