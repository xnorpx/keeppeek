use super::super::{InorderParser, SessionOptions, Timeline, UnknownRtcpSsrcPolicy};
use crate::{ConnectionContext, PacketContext, StreamContext, rtp::RawPacketBuilder};
use bytes::Bytes;

const SOURCE: u32 = 27;

fn receive(
    parser: &mut InorderParser,
    timeline: &mut Timeline,
    sequence_number: u16,
    timestamp: u32,
) {
    let (packet, _) = RawPacketBuilder {
        sequence_number,
        timestamp,
        payload_type: 96,
        ssrc: SOURCE,
        mark: true,
    }
    .build(*b"sample")
    .unwrap();
    assert!(
        parser
            .rtp(
                &SessionOptions::default(),
                &StreamContext::dummy(),
                None,
                &ConnectionContext::dummy(),
                &PacketContext::dummy(),
                timeline,
                0,
                packet.0
            )
            .unwrap()
            .is_some()
    );
}

fn sender_report(parser: &mut InorderParser, timeline: &mut Timeline, ssrc: u32, ntp: u64) {
    let mut bytes = [0_u8; 28];
    bytes[..4].copy_from_slice(&[0x80, 200, 0, 6]);
    bytes[4..8].copy_from_slice(&ssrc.to_be_bytes());
    bytes[8..16].copy_from_slice(&ntp.to_be_bytes());
    bytes[16..20].copy_from_slice(&337_u32.to_be_bytes());
    parser
        .rtcp(
            &SessionOptions::default(),
            &StreamContext::dummy(),
            None,
            &ConnectionContext::dummy(),
            &PacketContext::dummy(),
            timeline,
            0,
            Bytes::copy_from_slice(&bytes),
        )
        .unwrap();
}

#[test]
fn original_packet_clock_distinguishes_wrap_gap_backward_step_and_reconnect() {
    let mut timeline = Timeline::new(Some(u32::MAX - 15), 90_000, None).unwrap();
    let mut parser = InorderParser::new(Some(SOURCE), None, UnknownRtcpSsrcPolicy::Default);
    receive(&mut parser, &mut timeline, u16::MAX - 1, u32::MAX - 7);
    receive(&mut parser, &mut timeline, u16::MAX, u32::MAX - 7);
    receive(&mut parser, &mut timeline, 1, 32);
    receive(&mut parser, &mut timeline, 2, 30);
    let snapshot = parser.clock_snapshot(timeline.play_origin).unwrap();
    assert_eq!(snapshot.clock.first.timestamp(), i64::from(u32::MAX - 7));
    assert_eq!(snapshot.clock.latest.timestamp(), (1_i64 << 32) + 30);
    assert_eq!(snapshot.clock.backward_steps, 1);
    assert_eq!(snapshot.clock.missing_sequence_numbers, 1);
    assert_eq!(snapshot.packets, 4);
    assert_eq!(snapshot.play_origin, Some(u32::MAX - 15));
    let mut timeline = Timeline::new(None, 48_000, None).unwrap();
    let mut next = InorderParser::new(Some(SOURCE), None, UnknownRtcpSsrcPolicy::Default);
    assert!(next.clock_snapshot(timeline.play_origin).is_none());
    receive(&mut next, &mut timeline, 9, 1_001);
    let snapshot = next.clock_snapshot(timeline.play_origin).unwrap();
    assert_eq!(snapshot.clock.first, snapshot.clock.latest);
    assert_eq!(snapshot.clock.latest.timestamp(), 1_001);
    assert_eq!(snapshot.clock.backward_steps, 0);
    assert_eq!(snapshot.play_origin, None);
    assert_eq!(snapshot.sender_report, None);
}

#[test]
fn only_matching_nonzero_sender_reports_supply_a_clock_anchor() {
    let mut parser = InorderParser::new(Some(SOURCE), None, UnknownRtcpSsrcPolicy::ProcessPackets);
    let mut timeline = Timeline::new(None, 48_000, None).unwrap();
    let ntp = 0xe436_2f99_cccc_cccc;
    sender_report(&mut parser, &mut timeline, SOURCE, ntp);
    assert!(parser.clock_snapshot(None).is_none());
    receive(&mut parser, &mut timeline, 1, 200);
    sender_report(&mut parser, &mut timeline, SOURCE + 1, ntp + 1);
    assert_eq!(
        parser.clock_snapshot(None).unwrap().sender_report,
        Some((337, crate::NtpTimestamp(ntp)))
    );
    sender_report(&mut parser, &mut timeline, SOURCE, 0);
    assert_eq!(parser.clock_snapshot(None).unwrap().sender_report, None);
}
