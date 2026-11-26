use super::super::{InorderParser, SessionOptions, Timeline, UnknownRtcpSsrcPolicy};
use crate::{ConnectionContext, PacketContext, StreamContext, rtp::RawPacketBuilder};
use std::{hint::black_box, time::Instant};

#[test]
#[ignore = "run explicitly on both revisions to compare RTP ingest cost"]
fn measure_accepted_rtp_packets() {
    const PACKETS: u16 = 8_192;
    let source = 27;
    let data = (0..PACKETS)
        .map(|sequence_number| {
            let (packet, _) = RawPacketBuilder {
                sequence_number,
                timestamp: 97_000 + 3_000 * (u32::from(sequence_number) / 4),
                payload_type: 96,
                ssrc: source,
                mark: sequence_number % 4 == 3,
            }
            .build([0_u8; 1_024])
            .unwrap();
            packet.0
        })
        .collect::<Vec<_>>();
    let options = SessionOptions::default();
    let stream = StreamContext::dummy();
    let connection = ConnectionContext::dummy();
    let context = PacketContext::dummy();
    let mut nanos_per_packet = Vec::with_capacity(30);
    for run in 0..31 {
        let mut timeline = Timeline::new(Some(97_000), 90_000, None).unwrap();
        let mut parser = InorderParser::new(Some(source), None, 96, UnknownRtcpSsrcPolicy::Default);
        let start = Instant::now();
        for packet in &data {
            let result = parser
                .rtp(
                    &options,
                    &stream,
                    None,
                    &connection,
                    &context,
                    &mut timeline,
                    0,
                    packet.clone(),
                )
                .unwrap();
            assert!(black_box(result).is_some());
        }
        black_box(&parser);
        let elapsed = start.elapsed();
        if run > 0 {
            nanos_per_packet.push(elapsed.as_nanos() as f64 / f64::from(PACKETS));
        }
    }
    nanos_per_packet.sort_by(f64::total_cmp);
    crate::testutil::init_logging();
    log::info!(
        "8192 RTP packets, 30 runs: median {:.3} ns/packet, p95 {:.3} ns/packet",
        (nanos_per_packet[14] + nanos_per_packet[15]) / 2.0,
        nanos_per_packet[28]
    );
}
