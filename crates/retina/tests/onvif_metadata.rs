use retina::codec::{
    CodecItem, CompressionType, Depacketizer, FrameFormat, MessageFrame, ParameterSetInsertion,
    ParametersRef,
};
use retina::rtp::ReceivedPacketBuilder;
use retina::{PacketContext, Timestamp};
use std::num::NonZeroU32;

const DOCUMENT: &[u8] = b"<Event><State>true</State></Event>";
const GZIP_DOCUMENT: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xb3, 0x71, 0x2d, 0x4b, 0xcd, 0x2b,
    0xb1, 0xb3, 0x09, 0x2e, 0x49, 0x2c, 0x49, 0xb5, 0x2b, 0x29, 0x2a, 0x4d, 0xb5, 0xd1, 0x87, 0xb0,
    0x6d, 0xf4, 0x21, 0x52, 0x00, 0x3b, 0x76, 0x70, 0xce, 0x22, 0x00, 0x00, 0x00,
];
const ENCODINGS: [(&str, CompressionType, &[u8]); 5] = [
    (
        "vnd.onvif.metadata",
        CompressionType::Uncompressed,
        DOCUMENT,
    ),
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
        b"\x80\x00\x7f\x03",
    ),
    (
        "vnd.onvif.metadata.exi.ext",
        CompressionType::ExiInBand,
        b"\xa0\x00\x05\x01",
    ),
];

fn metadata(encoding: &str) -> Depacketizer {
    Depacketizer::new("application", encoding, 90_000, None, None).unwrap()
}

fn packet(sequence_number: u16, mark: bool) -> ReceivedPacketBuilder {
    ReceivedPacketBuilder {
        ctx: PacketContext::dummy(),
        stream_id: 2,
        sequence_number,
        timestamp: Timestamp::new(90_000, NonZeroU32::new(90_000).unwrap(), 0).unwrap(),
        payload_type: 107,
        ssrc: 42,
        mark,
        loss: 0,
    }
}

fn message(depacketizer: &mut Depacketizer) -> MessageFrame {
    let Some(Ok(CodecItem::MessageFrame(frame))) = depacketizer.pull() else {
        panic!("expected a complete metadata message");
    };
    assert_eq!(depacketizer.pull(), None);
    frame
}

#[test]
fn sdp_encodings_keep_compression_and_exi_modes_explicit() {
    for (encoding, expected, _) in ENCODINGS {
        let depacketizer = metadata(encoding);
        let Some(ParametersRef::Message(parameters)) = depacketizer.parameters() else {
            panic!("expected message parameters for {encoding}");
        };
        assert_eq!(parameters.compression_type(), expected, "{encoding}");
    }
    for unsupported in ["vnd.onvif.metadata+exi", "vnd.onvif.metadata.unknown"] {
        Depacketizer::new("application", unsupported, 90_000, None, None)
            .expect_err("unknown metadata encodings must not be guessed");
    }
}

#[test]
fn single_packet_documents_preserve_wire_bytes_and_context() {
    for (encoding, _, document) in ENCODINGS {
        let mut depacketizer = metadata(encoding);
        for sequence in 0..2 {
            let input = packet(sequence, true)
                .build(document.iter().copied())
                .unwrap();
            let expected_ctx = *input.ctx();
            let expected_timestamp = input.timestamp();
            depacketizer.push(input).unwrap();
            let frame = message(&mut depacketizer);
            assert_eq!(frame.data(), document);
            assert_eq!(*frame.ctx(), expected_ctx);
            assert_eq!(frame.timestamp(), expected_timestamp);
            assert_eq!(frame.stream_id(), 2);
            assert_eq!(frame.loss(), 0);
        }
    }
}

#[test]
fn every_split_and_final_marker_placement_preserves_wire_bytes() {
    for (encoding, _, document) in ENCODINGS {
        for split in 0..=document.len() {
            for empty_marker in [false, true] {
                let mut depacketizer = metadata(encoding);
                let first = packet(0, false)
                    .build(document[..split].iter().copied())
                    .unwrap();
                let expected_timestamp = first.timestamp();
                depacketizer.push(first).unwrap();
                assert_eq!(depacketizer.pull(), None);
                depacketizer
                    .push(
                        packet(1, !empty_marker)
                            .build(document[split..].iter().copied())
                            .unwrap(),
                    )
                    .unwrap();
                if empty_marker {
                    assert_eq!(depacketizer.pull(), None);
                    depacketizer
                        .push(packet(2, true).build([]).unwrap())
                        .unwrap();
                }
                let frame = message(&mut depacketizer);
                assert_eq!(frame.data(), document, "encoding={encoding}, split={split}");
                assert_eq!(frame.timestamp(), expected_timestamp);
                assert_eq!(frame.stream_id(), 2);
                assert_eq!(frame.loss(), 0);
                depacketizer
                    .push(packet(3, true).build(document.iter().copied()).unwrap())
                    .unwrap();
                assert_eq!(message(&mut depacketizer).data(), document);
            }
        }
    }
}

#[test]
fn one_byte_fragments_with_empty_packets_wait_for_marker() {
    for (encoding, _, document) in ENCODINGS {
        let mut depacketizer = metadata(encoding);
        for (index, byte) in document.iter().enumerate() {
            let sequence = u16::try_from(index * 2).unwrap();
            depacketizer
                .push(packet(sequence, false).build([*byte]).unwrap())
                .unwrap();
            assert_eq!(depacketizer.pull(), None);
            depacketizer
                .push(packet(sequence + 1, false).build([]).unwrap())
                .unwrap();
            assert_eq!(depacketizer.pull(), None);
        }
        let sequence = u16::try_from(document.len() * 2).unwrap();
        depacketizer
            .push(packet(sequence, true).build([]).unwrap())
            .unwrap();
        assert_eq!(message(&mut depacketizer).data(), document);
    }
}

fn assert_media_continues(video: &mut Depacketizer, audio: &mut Depacketizer, sequence: u16) {
    let video_packet = ReceivedPacketBuilder {
        stream_id: 0,
        payload_type: 96,
        ssrc: 40,
        ..packet(sequence, true)
    }
    .build([0x65, 0x88, 0x84])
    .unwrap();
    video.push(video_packet).unwrap();
    let Some(Ok(CodecItem::VideoFrame(frame))) = video.pull() else {
        panic!("expected uninterrupted H.264 output");
    };
    assert_eq!(frame.data(), &[0x00, 0x00, 0x00, 0x03, 0x65, 0x88, 0x84]);
    assert_eq!(frame.stream_id(), 0);
    assert_eq!(frame.loss(), 0);
    assert_eq!(video.pull(), None);

    let audio_packet = ReceivedPacketBuilder {
        stream_id: 1,
        timestamp: Timestamp::new(8_000, NonZeroU32::new(8_000).unwrap(), 0).unwrap(),
        payload_type: 0,
        ssrc: 41,
        ..packet(sequence, true)
    }
    .build([0x7f, 0xff, 0x00, 0x80])
    .unwrap();
    audio.push(audio_packet).unwrap();
    let Some(Ok(CodecItem::AudioFrame(frame))) = audio.pull() else {
        panic!("expected uninterrupted PCMU output");
    };
    assert_eq!(frame.data(), &[0x7f, 0xff, 0x00, 0x80]);
    assert_eq!(frame.stream_id(), 1);
    assert_eq!(frame.loss(), 0);
    assert_eq!(audio.pull(), None);
}

#[test]
fn metadata_faults_leave_interleaved_video_and_audio_running() {
    let mut video = Depacketizer::new(
        "video", "h264", 90_000, None,
        Some("packetization-mode=1;profile-level-id=64001E;sprop-parameter-sets=Z2QAHqwsaoLA9puCgIKgAAADACAAAAMD0IAA,aO4xshsA"),
    ).unwrap();
    video.set_frame_format(FrameFormat {
        parameter_set_insertion: ParameterSetInsertion::Never,
        ..FrameFormat::MP4
    });
    let mut audio = Depacketizer::new("audio", "pcmu", 8_000, None, None).unwrap();
    let mut depacketizer = metadata("vnd.onvif.metadata");
    assert_media_continues(&mut video, &mut audio, 0);
    depacketizer
        .push(packet(0, false).build(*b"<Event>").unwrap())
        .unwrap();
    assert_eq!(depacketizer.pull(), None);
    let mut changed = packet(1, false);
    changed.timestamp = Timestamp::new(180_000, NonZeroU32::new(90_000).unwrap(), 0).unwrap();
    depacketizer
        .push(changed.build(*b"invalid").unwrap())
        .unwrap();
    assert_eq!(depacketizer.pull(), None);
    assert_media_continues(&mut video, &mut audio, 1);
    let mut lost = packet(3, true);
    lost.loss = 1;
    depacketizer
        .push(lost.build(*b"</Event>").unwrap())
        .unwrap();
    assert_eq!(depacketizer.pull(), None);
    let block = vec![0x5a; 32 * 1024];
    for sequence in 4..13 {
        depacketizer
            .push(
                packet(sequence, sequence == 12)
                    .build(block.iter().copied())
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(depacketizer.pull(), None);
        assert_media_continues(&mut video, &mut audio, sequence);
    }
    depacketizer
        .push(packet(13, true).build(DOCUMENT.iter().copied()).unwrap())
        .unwrap();
    let frame = message(&mut depacketizer);
    assert_eq!(frame.data(), DOCUMENT);
    assert_eq!(frame.loss(), 1);
    assert_media_continues(&mut video, &mut audio, 13);
}
