use crate::webrtc::{
    Publisher, TalkbackRoute,
    audio::{AudioCodec, AudioFrame},
};
use reo_proto::{
    header::PacketHeader,
    session::{BcSession, Command, Input},
    stream::{StreamSubscription, StreamType},
    talk::{ImaAdpcmEncoder, TalkAbility, TalkCommand, TalkConfig, TalkEvent},
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const MAX_AGE: Duration = Duration::from_millis(250);

pub(super) struct Talkback {
    channel: u8,
    stream_id: Option<u32>,
    route: Option<TalkbackRoute>,
    queued_route: Option<TalkbackRoute>,
    pending: Option<AudioFrame>,
    blocks: Option<SpeakerBlocks>,
    ready: bool,
    sequence: u16,
}

impl Talkback {
    pub(super) const fn new(channel: u8) -> Self {
        Self {
            channel,
            stream_id: None,
            route: None,
            queued_route: None,
            pending: None,
            blocks: None,
            ready: false,
            sequence: 0,
        }
    }

    pub(super) fn subscribed(
        &mut self,
        id: u32,
        session: &mut BcSession,
    ) -> Result<(), reo_proto::BcError> {
        self.stream_id = Some(id);
        session.handle_input(Input::Command(Command::Talk(TalkCommand::QueryAbility {
            channel: self.channel,
        })))
    }

    pub(super) fn observe(
        &mut self,
        event: TalkEvent,
        session: &mut BcSession,
    ) -> Result<(), reo_proto::BcError> {
        match event {
            TalkEvent::Ability(ability) if self.stream_id.is_some() => {
                match select_profile(*ability, self.channel) {
                    Ok(config) => {
                        self.blocks = Some(SpeakerBlocks::new(&config));
                        session.handle_input(Input::Command(Command::Talk(
                            TalkCommand::Configure(config),
                        )))?;
                    }
                    Err(error) => {
                        tracing::warn!(channel = self.channel, %error, "no compatible Reolink speaker profile");
                    }
                }
            }
            TalkEvent::Configured => self.ready = self.blocks.is_some(),
            TalkEvent::Reset if self.stream_id.is_some() => {
                self.ready = false;
                self.blocks = None;
                self.pending = None;
                self.sequence = 0;
                session.handle_input(Input::Command(Command::Talk(TalkCommand::QueryAbility {
                    channel: self.channel,
                })))?;
            }
            TalkEvent::Reset | TalkEvent::Ability(_) => {}
        }
        Ok(())
    }

    pub(super) fn close(&mut self, session: &mut BcSession) -> Result<(), reo_proto::BcError> {
        self.pending = None;
        self.blocks = None;
        self.ready = false;
        // An outstanding subscription reserves the route until its stream ID arrives.
        if let Some(stream_id) = self.stream_id.take() {
            self.route = None;
            session.handle_input(Input::Command(Command::Talk(TalkCommand::Reset {
                channel: self.channel,
            })))?;
            session.handle_input(Input::Command(Command::UnsubscribeStream { stream_id }))?;
        }
        Ok(())
    }

    // The actor drains protocol output before drive(), and SendAdpcm queues
    // exactly one packet. poll_output drains sends before events or deadlines.
    // Keep this check before another protocol input can coalesce packets.
    pub(super) fn flush(
        &mut self,
        session: &mut BcSession,
        buffer: &mut [u8],
        wire: &mut dyn super::BaichuanTransport,
        live: &Publisher,
        source: &str,
    ) -> anyhow::Result<()> {
        let Some(route) = self.queued_route.take() else {
            return Ok(());
        };
        let reo_proto::session::Output::TcpSend { data } = session.poll_output(buffer)? else {
            anyhow::bail!("Reolink speaker packet missing from transport output");
        };
        let (header, header_len) = PacketHeader::parse(data)?;
        anyhow::ensure!(
            header.msg_id == reo_proto::COMMAND_TALK
                && data.len() == header_len + header.body_len as usize,
            "Reolink speaker output did not contain exactly one complete packet"
        );
        if live.talkback_route_is_current(source, &route) {
            wire.send(data)?;
        }
        Ok(())
    }

    pub(super) fn drive(
        &mut self,
        session: &mut BcSession,
        live: &Publisher,
        source: &str,
        now: Instant,
    ) -> Result<Option<Instant>, reo_proto::BcError> {
        if self
            .route
            .as_ref()
            .is_some_and(|route| !live.talkback_route_is_current(source, route))
        {
            self.close(session)?;
        }
        if self.route.is_none()
            && let Some((route, frame)) = live.take_routed_talkback_audio(source, None)
        {
            self.route = Some(route);
            self.pending = Some(frame);
            self.sequence = 0;
            self.ready = false;
            session.handle_input(Input::Command(Command::SubscribeStream(
                StreamSubscription {
                    channel: self.channel,
                    stream_type: StreamType::Extern,
                    expected_width: 0,
                    expected_height: 0,
                },
            )))?;
        }
        if !self.ready {
            return Ok(None);
        }
        let (Some(blocks), Some(route)) = (&mut self.blocks, &self.route) else {
            return Ok(None);
        };
        for _ in 0..4 {
            let Some(frame) = self.pending.take().or_else(|| {
                live.take_routed_talkback_audio(source, Some(route))
                    .map(|(_, frame)| frame)
            }) else {
                break;
            };
            blocks.push(&frame, now);
        }
        if let Some(data) = blocks.next_block(now)? {
            session.handle_input(Input::Command(Command::Talk(TalkCommand::SendAdpcm {
                channel: self.channel,
                sequence: self.sequence,
                data,
            })))?;
            self.sequence = self.sequence.wrapping_add(1);
            self.queued_route = Some(route.clone());
            return Ok(Some(now));
        }
        Ok(Some(now + Duration::from_millis(20)))
    }
}

fn select_profile(mut ability: TalkAbility, channel: u8) -> Result<TalkConfig, reo_proto::BcError> {
    ability.audio_profiles.retain(|profile| {
        matches!(profile.sample_rate, 8_000 | 16_000)
            && profile.sample_precision == 16
            && profile.sound_track.eq_ignore_ascii_case("mono")
            && profile.length_per_encoder <= profile.sample_rate / 4
    });
    ability.select_adpcm(channel)
}

/// Joins short WebRTC microphone packets into one paced camera ADPCM block.
struct SpeakerBlocks {
    rate: u32,
    block_len: usize,
    period: Duration,
    samples: VecDeque<(Instant, i16)>,
    encoder: ImaAdpcmEncoder,
    next_send: Option<Instant>,
}

impl SpeakerBlocks {
    fn new(config: &TalkConfig) -> Self {
        let profile = &config.audio_profile;
        let rate = profile.sample_rate;
        let block_len = profile.length_per_encoder as usize;
        debug_assert!(matches!(rate, 8_000 | 16_000));
        debug_assert!(
            block_len >= 2 && block_len <= rate as usize / 4 && block_len.is_multiple_of(2)
        );
        Self {
            rate,
            block_len,
            period: Duration::from_nanos(
                u64::from(profile.length_per_encoder) * 1_000_000_000 / u64::from(rate),
            ),
            samples: VecDeque::new(),
            encoder: ImaAdpcmEncoder::default(),
            next_send: None,
        }
    }

    fn push(&mut self, frame: &AudioFrame, now: Instant) {
        // The negotiated browser microphone is mono G.711 at 8 kHz. A 16 kHz
        // Reolink profile consumes two output samples per decoded microphone sample.
        if frame.codec != AudioCodec::PcmS16Le
            || frame.channel_count != 1
            || !(frame.sample_rate_hz == self.rate || frame.sample_rate_hz == 8_000)
            || now.saturating_duration_since(frame.received_at) > MAX_AGE
            || !frame.data.len().is_multiple_of(2)
            || frame.data.len() > frame.sample_rate_hz as usize / 2
            || self
                .samples
                .back()
                .is_some_and(|(last, _)| *last > frame.received_at)
        {
            return;
        }
        let repeats = self.rate / frame.sample_rate_hz;
        let incoming = frame.data.len() / 2 * repeats as usize;
        let excess = (self.samples.len() + incoming).saturating_sub(self.rate as usize / 4);
        self.samples.drain(..excess);
        for sample in frame.data.as_chunks::<2>().0 {
            let sample = i16::from_le_bytes(*sample);
            self.samples.extend(std::iter::repeat_n(
                (frame.received_at, sample),
                repeats as usize,
            ));
        }
    }

    fn next_block(&mut self, now: Instant) -> Result<Option<Vec<u8>>, reo_proto::BcError> {
        while self
            .samples
            .front()
            .is_some_and(|(received, _)| now.saturating_duration_since(*received) > MAX_AGE)
        {
            self.samples.pop_front();
        }
        if self.samples.len() < self.block_len || self.next_send.is_some_and(|next| now < next) {
            return Ok(None);
        }
        let pcm: Vec<_> = self
            .samples
            .drain(..self.block_len)
            .map(|(_, sample)| sample)
            .collect();
        let mut block = vec![0; 4 + self.block_len / 2];
        self.encoder.encode_block(&pcm, &mut block)?;
        self.next_send = Some(now + self.period);
        Ok(Some(block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use reo_proto::talk::TalkAudioProfile;

    fn speaker(rate: u32, len: u32) -> SpeakerBlocks {
        SpeakerBlocks::new(&TalkConfig {
            channel: 0,
            duplex: Default::default(),
            audio_stream_mode: Default::default(),
            audio_profile: TalkAudioProfile {
                sample_rate: rate,
                length_per_encoder: len,
                ..Default::default()
            },
        })
    }

    fn packet(samples: &[i16], at: Instant) -> AudioFrame {
        AudioFrame {
            codec: AudioCodec::PcmS16Le,
            sample_rate_hz: 8_000,
            channel_count: 1,
            timestamp: None,
            received_at: at,
            data: Bytes::from(
                samples
                    .iter()
                    .flat_map(|sample| sample.to_le_bytes())
                    .collect::<Vec<_>>(),
            ),
        }
    }

    #[test]
    fn speaker_reset_discards_negotiated_audio_and_requests_fresh_ability() {
        use reo_proto::session::{BcSessionConfig, Output};

        let now = Instant::now();
        let mut session = BcSession::new(BcSessionConfig::default_client(), now);
        let mut transport_output = vec![0; reo_proto::DEFAULT_MEDIA_OUTPUT_BUFFER_SIZE];
        let mut talkback = Talkback::new(2);
        session
            .handle_input(Input::Command(Command::SubscribeStream(
                StreamSubscription {
                    channel: 2,
                    stream_type: StreamType::Extern,
                    expected_width: 0,
                    expected_height: 0,
                },
            )))
            .unwrap();
        talkback.subscribed(1, &mut session).unwrap();
        while !matches!(
            session.poll_output(&mut transport_output).unwrap(),
            Output::Timeout(_)
        ) {}

        talkback.blocks = Some(speaker(8_000, 320));
        talkback.pending = Some(packet(&[1, 2], now));
        talkback.sequence = 9;
        talkback.ready = true;
        talkback.observe(TalkEvent::Reset, &mut session).unwrap();

        assert!(!talkback.ready);
        assert!(talkback.blocks.is_none());
        assert!(talkback.pending.is_none());
        assert_eq!(talkback.sequence, 0);
        let Output::TcpSend { data } = session.poll_output(&mut transport_output).unwrap() else {
            panic!("expected fresh capabilities request");
        };
        assert_eq!(
            PacketHeader::parse(data).unwrap().0.msg_id,
            reo_proto::COMMAND_TALK_CAPABILITIES
        );
    }

    #[test]
    fn rejects_unplayable_talkback_profiles_before_allocating_blocks() {
        let mut ability = TalkAbility::default();
        ability.duplex_modes.push("fullDuplex".try_into().unwrap());
        ability
            .audio_stream_modes
            .push("speaker".try_into().unwrap());
        let valid = TalkAudioProfile {
            audio_type: "adpcm".try_into().unwrap(),
            sample_rate: 16_000,
            sample_precision: 16,
            length_per_encoder: 1_016,
            sound_track: "mono".try_into().unwrap(),
        };
        for invalid in [
            TalkAudioProfile {
                sample_rate: 48_000,
                ..valid.clone()
            },
            TalkAudioProfile {
                length_per_encoder: 4_002,
                ..valid.clone()
            },
            TalkAudioProfile {
                length_per_encoder: 1,
                ..valid.clone()
            },
            TalkAudioProfile {
                length_per_encoder: 3,
                ..valid.clone()
            },
            TalkAudioProfile {
                sample_precision: 8,
                ..valid.clone()
            },
            TalkAudioProfile {
                sound_track: "stereo".try_into().unwrap(),
                ..valid.clone()
            },
        ] {
            ability.audio_profiles.clear();
            ability.audio_profiles.push(invalid);
            assert!(select_profile(ability.clone(), 0).is_err());
            ability.audio_profiles.push(valid.clone());
            assert_eq!(
                select_profile(ability.clone(), 0).unwrap().audio_profile,
                valid
            );
        }
    }

    #[test]
    fn reolink_talkback_assembles_short_microphone_packets_in_negotiated_blocks() {
        let now = Instant::now();
        for (rate, len) in [(8_000, 1_016), (16_000, 640), (16_000, 1_016)] {
            let mut output = speaker(rate, len);
            let mut reference = ImaAdpcmEncoder::default();
            let samples: Vec<i16> = (0..1_920).map(|i| ((i * 43) % 24_000) as i16).collect();
            let expected: Vec<_> = samples
                .iter()
                .flat_map(|s| std::iter::repeat_n(*s, (rate / 8_000) as usize))
                .collect();
            let mut sent = 0;
            for (i, pcm) in samples.chunks(160).enumerate() {
                let at = now + Duration::from_millis(20 * i as u64);
                output.push(&packet(pcm, at), at);
                if let Some(block) = output.next_block(at).unwrap() {
                    let mut golden = vec![0; 4 + len as usize / 2];
                    reference
                        .encode_block(&expected[sent..sent + len as usize], &mut golden)
                        .unwrap();
                    assert_eq!(block, golden);
                    sent += len as usize;
                    assert!(output.next_block(at).unwrap().is_none());
                }
            }
            assert!(sent >= len as usize);
            assert_eq!(output.samples.len(), expected.len() - sent);
        }
    }

    #[test]
    fn reolink_talkback_bounds_backlog_expires_audio_and_paces_from_actual_send() {
        let now = Instant::now();
        let mut output = speaker(8_000, 640);
        let full = [1_i16; 2_000];
        output.push(&packet(&full, now), now);
        output.push(&packet(&full, now), now);
        assert_eq!(output.samples.len(), 2_000);
        assert!(output.next_block(now).unwrap().is_some());
        assert!(
            output
                .next_block(now + Duration::from_millis(79))
                .unwrap()
                .is_none()
        );
        assert!(
            output
                .next_block(now + Duration::from_millis(170))
                .unwrap()
                .is_some()
        );
        assert!(
            output
                .next_block(now + Duration::from_millis(171))
                .unwrap()
                .is_none()
        );
        let late = now + MAX_AGE + Duration::from_millis(1);
        assert!(output.next_block(late).unwrap().is_none());
        output.push(&packet(&full, now), late);
        assert!(output.samples.is_empty());
    }
}
