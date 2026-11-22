use retina::client::{RtpClockSnapshot, Stream};
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Debug)]
pub(super) struct SourceTiming {
    video: Option<TrackTiming>,
    audio: Option<TrackTiming>,
    latest_audio_minus_video_play_us: Option<i64>,
}

#[derive(Debug)]
struct TrackTiming {
    track_index: usize,
    clock_rate_hz: u32,
    play_origin_ticks: Option<u32>,
    first_rtp_ticks: u32,
    latest_rtp_ticks: u32,
    first_extended_ticks: i64,
    latest_extended_ticks: i64,
    accepted_packets: u64,
    missing_sequence_numbers: u64,
    backward_timestamp_steps: u64,
    sender_report: Option<SenderReport>,
}

#[derive(Debug)]
struct SenderReport {
    rtp_ticks: u32,
    ntp_seconds: u32,
    ntp_fraction: u32,
}

const COLLECTION_WINDOW: Duration = Duration::from_secs(300);

fn within_collection_window(start: &OnceLock<Instant>, now: Instant) -> bool {
    now.saturating_duration_since(*start.get_or_init(|| now)) < COLLECTION_WINDOW
}

pub(super) fn periodic_enabled() -> bool {
    static START: OnceLock<Instant> = OnceLock::new();
    tracing::enabled!(tracing::Level::TRACE) && within_collection_window(&START, Instant::now())
}

// Four exact u32 words survive both JSON and browser number representations.
const fn uuid_words(uuid: Uuid) -> [u32; 4] {
    let (high, low) = uuid.as_u64_pair();
    [
        (high >> 32) as u32,
        high as u32,
        (low >> 32) as u32,
        low as u32,
    ]
}

impl SourceTiming {
    pub(super) fn from_tracks(tracks: &[Stream]) -> Option<Self> {
        let unique = |kind| {
            let mut candidates = tracks.iter().enumerate().filter_map(|(index, track)| {
                (track.media() == kind && track.ctx().is_some()).then_some((index, track))
            });
            let (index, track) = candidates.next()?;
            if candidates.next().is_some() {
                return None;
            }
            Some(TrackTiming::new(index, track.rtp_clock()?))
        };
        Self::new(unique("video"), unique("audio"))
    }

    pub(super) fn emit(
        &self,
        camera_id: Option<Uuid>,
        stream: crate::keeppeek::StreamKind,
        session: Uuid,
        phase: &'static str,
    ) {
        let camera = camera_id.map(uuid_words);
        let session = uuid_words(session);
        for (kind, track) in [
            ("video", self.video.as_ref()),
            ("audio", self.audio.as_ref()),
        ] {
            let Some(track) = track else {
                continue;
            };
            let report = track.sender_report.as_ref();
            tracing::info!(
                name: "camera.rtsp.source_timing",
                camera_registered = camera.is_some(),
                camera_uuid_0 = camera.map(|v| v[0]), camera_uuid_1 = camera.map(|v| v[1]),
                camera_uuid_2 = camera.map(|v| v[2]), camera_uuid_3 = camera.map(|v| v[3]),
                session_uuid_0 = session[0], session_uuid_1 = session[1],
                session_uuid_2 = session[2], session_uuid_3 = session[3], stream = %stream, phase, kind,
                track_index = track.track_index as u64,
                clock_rate_hz = track.clock_rate_hz,
                play_origin_ticks = track.play_origin_ticks,
                first_rtp_ticks = track.first_rtp_ticks, latest_rtp_ticks = track.latest_rtp_ticks,
                first_extended_ticks = track.first_extended_ticks, latest_extended_ticks = track.latest_extended_ticks,
                accepted_packets = track.accepted_packets, missing_sequence_numbers = track.missing_sequence_numbers,
                backward_timestamp_steps = track.backward_timestamp_steps,
                sender_rtp_ticks = report.map(|r| r.rtp_ticks),
                sender_ntp_seconds = report.map(|r| r.ntp_seconds),
                sender_ntp_fraction = report.map(|r| r.ntp_fraction),
                latest_audio_minus_video_play_us = self.latest_audio_minus_video_play_us,
                "RTSP source timing"
            );
        }
    }

    fn new(video: Option<TrackTiming>, audio: Option<TrackTiming>) -> Option<Self> {
        if video.is_none() && audio.is_none() {
            return None;
        }
        let latest_audio_minus_video_play_us = audio.as_ref().and_then(|audio| {
            let difference = audio.play_us()? - video.as_ref()?.play_us()?;
            i64::try_from(difference).ok()
        });
        Some(Self {
            video,
            audio,
            latest_audio_minus_video_play_us,
        })
    }
}

impl TrackTiming {
    fn new(track_index: usize, snapshot: RtpClockSnapshot) -> Self {
        let first = snapshot.clock.first;
        let latest = snapshot.clock.latest;
        Self {
            track_index,
            clock_rate_hz: latest.clock_rate().get(),
            play_origin_ticks: snapshot.play_origin,
            // The wire timestamp is the low 32 bits of the extended RTP clock.
            first_rtp_ticks: first.timestamp() as u32,
            latest_rtp_ticks: latest.timestamp() as u32,
            first_extended_ticks: first.timestamp(),
            latest_extended_ticks: latest.timestamp(),
            accepted_packets: snapshot.packets,
            missing_sequence_numbers: snapshot.clock.missing_sequence_numbers,
            backward_timestamp_steps: snapshot.clock.backward_steps,
            sender_report: snapshot.sender_report.map(|(rtp_ticks, ntp)| SenderReport {
                rtp_ticks,
                ntp_seconds: (ntp.0 >> 32) as u32,
                ntp_fraction: ntp.0 as u32,
            }),
        }
    }

    fn play_us(&self) -> Option<i128> {
        let ticks = i128::from(self.latest_extended_ticks) - i128::from(self.play_origin_ticks?);
        Some(ticks * 1_000_000 / i128::from(self.clock_rate_hz))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retina::{Timestamp, client::rtp::PacketClock};
    use std::num::NonZeroU32;

    fn track(rate: u32, origin: Option<u32>, latest: i64) -> TrackTiming {
        let base = origin.unwrap_or(100_000);
        let rate = NonZeroU32::new(rate).unwrap();
        let first = Timestamp::new(i64::from(base), rate, base).unwrap();
        let latest = Timestamp::new(latest, rate, base).unwrap();
        TrackTiming::new(
            0,
            RtpClockSnapshot {
                packets: 2,
                play_origin: origin,
                clock: PacketClock {
                    first,
                    latest,
                    backward_steps: 0,
                    missing_sequence_numbers: 0,
                },
                sender_report: None,
            },
        )
    }

    #[test]
    fn recorder_default_filter_keeps_periodic_collection_off() {
        use tracing_subscriber::{EnvFilter, prelude::*};
        let subscriber = tracing_subscriber::Registry::default()
            .with(EnvFilter::new(crate::logging::DEFAULT_LOG_FILTER));
        tracing::subscriber::with_default(subscriber, || assert!(!periodic_enabled()));
        let subscriber = tracing_subscriber::Registry::default()
            .with(EnvFilter::new("warn,keeppeek::rtsp::timing=trace"));
        tracing::subscriber::with_default(subscriber, || assert!(periodic_enabled()));
    }

    #[test]
    fn periodic_collection_stops_at_its_single_recorder_window() {
        let start = OnceLock::new();
        let now = Instant::now();
        assert!(within_collection_window(&start, now));
        assert!(within_collection_window(
            &start,
            now + COLLECTION_WINDOW - Duration::from_millis(1)
        ));
        assert!(!within_collection_window(&start, now + COLLECTION_WINDOW));
        assert!(!within_collection_window(
            &start,
            now + COLLECTION_WINDOW * 2
        ));
    }

    #[test]
    fn uuid_identity_uses_four_exact_json_numbers() {
        let id = Uuid::parse_str("00112233-4455-4677-8899-aabbccddeeff").unwrap();
        assert_eq!(
            uuid_words(id),
            [0x0011_2233, 0x4455_4677, 0x8899_aabb, 0xccdd_eeff]
        );
    }

    #[test]
    fn relative_packet_positions_use_both_play_origins_and_respect_sign_and_wrap() {
        let video = || Some(track(90_000, Some(u32::MAX), i64::from(u32::MAX) + 90_000));
        for (audio_ticks, expected) in [(42_400, -200_000), (52_000, 0), (61_600, 200_000)] {
            let audio = Some(track(48_000, Some(4_000), audio_ticks));
            let report = SourceTiming::new(video(), audio).unwrap();
            assert_eq!(report.latest_audio_minus_video_play_us, Some(expected));
            assert_eq!(report.video.unwrap().latest_rtp_ticks, 89_999);
        }
        let audio = Some(track(48_000, None, 148_000));
        assert_eq!(
            SourceTiming::new(video(), audio)
                .unwrap()
                .latest_audio_minus_video_play_us,
            None
        );
        assert_eq!(
            SourceTiming::new(video(), None)
                .unwrap()
                .latest_audio_minus_video_play_us,
            None
        );
        assert!(SourceTiming::new(None, None).is_none());
    }
}
