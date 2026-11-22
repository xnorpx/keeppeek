use retina::client::{RtpClockSnapshot, Stream};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct SourceTiming {
    video: Option<TrackTiming>,
    audio: Option<TrackTiming>,
    latest_audio_minus_video_play_us: Option<i64>,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
struct SenderReport {
    rtp_ticks: u32,
    ntp_seconds: u32,
    ntp_fraction: u32,
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
