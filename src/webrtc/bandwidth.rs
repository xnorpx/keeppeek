use super::*;

mod connection;
mod diagnostics;

pub(super) use diagnostics::QualitySnapshot;

#[derive(Debug, Default)]
pub(super) struct SessionBandwidth {
    transport_estimate_bps: Option<u64>,
    desired_bitrate_bps: Option<u64>,
    refresh_due: Option<Instant>,
    quality_report_due: Option<Instant>,
    pub(super) dirty: bool,
    connection: connection::ConnectionReporting,
}

#[derive(Default)]
struct BandwidthDemand {
    desired_bps: u64,
    reserved_bps: u64,
    automatic_tracks: u64,
    active_bps: u64,
    minimum_bps: u64,
    has_rtp_tracks: bool,
}

impl ApiMediaRuntime {
    fn observe_bandwidth(&mut self, estimate: &BweKind, _now: Instant) {
        match estimate {
            BweKind::Twcc(bitrate) => {
                // str0m emits changed values, so a stable estimate has no event-age deadline.
                self.bandwidth.transport_estimate_bps = Some(bitrate.as_u64());
                self.bandwidth.dirty = true;
            }
            BweKind::Remb(mid, bitrate) => {
                if let Some(track) = self
                    .tracks
                    .iter_mut()
                    .find(|track| track.runtime.delivery.mid() == Some(*mid))
                {
                    track.runtime.receiver_limit_bps = Some(bitrate.as_u64());
                    self.bandwidth.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn refresh_bandwidth(&mut self, rtc: &mut Rtc, now: Instant) {
        if !self.bandwidth.dirty
            && self.bandwidth.desired_bitrate_bps.is_some()
            && self.bandwidth.refresh_due.is_none_or(|due| now < due)
        {
            return;
        }
        let demand = self.bandwidth_demand();
        let desired_bps = demand.desired_bps.min(MAX_DESIRED_BITRATE.as_u64());
        if self.bandwidth.desired_bitrate_bps != Some(desired_bps) {
            rtc.bwe().set_desired_bitrate(Bitrate::bps(desired_bps));
            self.bandwidth.desired_bitrate_bps = Some(desired_bps);
        }
        let available_bps = self
            .bandwidth
            .transport_estimate_bps
            .unwrap_or(0)
            .saturating_sub(demand.reserved_bps);
        let automatic_bps = available_bps
            .checked_div(demand.automatic_tracks)
            .unwrap_or(0);
        for track in &self.tracks {
            if track.runtime.delivery.mid().is_none() {
                continue;
            }
            let allocation_bps = if adaptive_auto(&track.runtime.subscription) {
                track
                    .runtime
                    .receiver_limit_bps
                    .map_or(automatic_bps, |limit| automatic_bps.min(limit))
            } else {
                0
            };
            track
                .runtime
                .subscription
                .update_estimate(Bitrate::bps(allocation_bps));
        }
        self.report_connection(now, &demand);
        self.report_live_quality(now);
        self.bandwidth.dirty = false;
        self.bandwidth.refresh_due = (!self.tracks.is_empty()
            || demand.has_rtp_tracks
            || self.bandwidth.transport_estimate_bps.is_some())
        .then(|| now + DESIRED_BITRATE_REFRESH);
    }

    fn bandwidth_demand(&self) -> BandwidthDemand {
        let mut demand = BandwidthDemand::default();
        // ponytail: SDP admission bounds these linear scans; add an index only after measurement.
        for track in &self.tracks {
            if track.runtime.delivery.mid().is_none() {
                continue;
            }
            demand.has_rtp_tracks = true;
            let subscription = &track.runtime.subscription;
            let quality = subscription
                .requested_quality()
                .unwrap_or(StreamQuality::High);
            let desired_bps = subscription.desired_bitrate(quality).as_u64();
            let active_required_bps = subscription
                .desired_source_bitrate(subscription.active_source)
                .as_u64();
            let retained_bps = desired_bps.max(active_required_bps);
            demand.active_bps = demand.active_bps.saturating_add(
                subscription
                    .source_bitrate(subscription.active_source)
                    .as_u64(),
            );
            demand.desired_bps = demand.desired_bps.saturating_add(retained_bps);
            if adaptive_auto(subscription) {
                demand.automatic_tracks += 1;
                let low = subscription
                    .low_source
                    .expect("adaptive Auto has a substream");
                demand.minimum_bps = demand
                    .minimum_bps
                    .saturating_add(subscription.source_bitrate(low).as_u64());
            } else {
                demand.reserved_bps = demand.reserved_bps.saturating_add(retained_bps);
                demand.minimum_bps = demand.minimum_bps.saturating_add(desired_bps);
            }
        }
        for track in &self.audio_tracks {
            if track.runtime.delivery.mid().is_some() {
                demand.has_rtp_tracks = true;
                demand.active_bps = demand.active_bps.saturating_add(track.reserved_bitrate_bps);
                demand.minimum_bps = demand
                    .minimum_bps
                    .saturating_add(track.reserved_bitrate_bps);
                demand.desired_bps = demand
                    .desired_bps
                    .saturating_add(track.reserved_bitrate_bps);
                demand.reserved_bps = demand
                    .reserved_bps
                    .saturating_add(track.reserved_bitrate_bps);
            }
        }
        demand
    }
}

fn adaptive_auto(subscription: &SourceSubscription) -> bool {
    subscription.requested_quality() == Some(StreamQuality::Auto)
        && subscription.low_source.is_some()
}

pub(super) fn audio_reservation_bps(format: &crate::api::proto::MediaDataFormat) -> u64 {
    let Some(crate::api::proto::media_data_format::Format::Audio(audio)) = &format.format else {
        return 128_000;
    };
    // G711 needs 64 kbps of payload; reserve overhead even when the source format is not yet known.
    u64::from(audio.sample_rate_hz)
        .saturating_mul(u64::from(audio.channel_count))
        .saturating_mul(8)
        .saturating_mul(5)
        .div_euclid(4)
        .max(128_000)
}

fn observe_api_output(rtc: &mut Rtc, media: &mut ApiMediaRuntime, output: &Output, now: Instant) {
    if let Output::Event(Event::EgressBitrateEstimate(estimate)) = output {
        media.observe_bandwidth(estimate, now);
        media.refresh_bandwidth(rtc, now);
    }
}

pub(super) fn poll_api_output(
    rtc: &mut Rtc,
    media: &mut ApiMediaRuntime,
    now: Instant,
) -> anyhow::Result<Output> {
    media.refresh_bandwidth(rtc, now);
    let mut output = rtc.poll_output()?;
    observe_api_output(rtc, media, &output, now);
    if let Output::Timeout(deadline) = &mut output
        && let Some(refresh_due) = media.bandwidth.refresh_due
    {
        *deadline = (*deadline).min(refresh_due);
    }
    Ok(output)
}

#[cfg(test)]
mod tests;
