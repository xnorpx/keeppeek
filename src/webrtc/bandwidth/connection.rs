use super::*;

#[derive(Debug, Default)]
pub(super) struct ConnectionReporting {
    last: Option<ConnectionSnapshot>,
    next: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConnectionSnapshot {
    estimate_bps: u64,
    limited: bool,
    update_required: bool,
}

impl ApiMediaRuntime {
    pub(super) fn report_connection(&mut self, now: Instant, demand: &BandwidthDemand) {
        let Some(estimate_bps) = self.bandwidth.transport_estimate_bps else {
            return;
        };
        if self
            .bandwidth
            .connection
            .next
            .is_some_and(|next| now < next)
        {
            return;
        }
        let snapshot = ConnectionSnapshot {
            estimate_bps,
            limited: demand.active_bps > estimate_bps || demand.minimum_bps > estimate_bps,
            update_required: demand.minimum_bps > estimate_bps,
        };
        if self.bandwidth.connection.last == Some(snapshot) {
            return;
        }
        self.bandwidth.connection.next = Some(now + DESIRED_BITRATE_REFRESH);
        let notification = crate::api::proto::Notification {
            event: Some(crate::api::proto::notification::Event::ConnectionUpdate(
                crate::api::proto::ConnectionUpdate {
                    state: if snapshot.limited {
                        crate::api::proto::ConnectionState::LimitedConnectivity
                    } else {
                        crate::api::proto::ConnectionState::Healthy
                    } as i32,
                    available_bitrate_bps: estimate_bps,
                    subscription_update_required: snapshot.update_required,
                },
            )),
        };
        match self.enqueue_control_notification(notification) {
            Ok(()) => self.bandwidth.connection.last = Some(snapshot),
            Err(error) => tracing::warn!(%error, "deferring connection bandwidth notification"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::proto::{ConnectionState, Notification, notification};

    fn demand(active_bps: u64, minimum_bps: u64) -> BandwidthDemand {
        BandwidthDemand {
            active_bps,
            minimum_bps,
            ..Default::default()
        }
    }

    fn update(media: &ApiMediaRuntime) -> crate::api::proto::ConnectionUpdate {
        let event = media
            .control_notifications
            .back()
            .unwrap()
            .event
            .as_ref()
            .unwrap();
        let notification::Event::ConnectionUpdate(update) = event else {
            panic!("expected a connection update");
        };
        *update
    }

    #[test]
    fn connection_feedback_distinguishes_unknown_from_zero() {
        let mut media = ApiMediaRuntime::default();
        let now = Instant::now();
        media.report_connection(now, &demand(500_000, 500_000));
        assert!(media.control_notifications.is_empty());
        media.bandwidth.transport_estimate_bps = Some(0);
        media.report_connection(now, &demand(500_000, 500_000));
        let update = update(&media);
        assert_eq!(update.available_bitrate_bps, 0);
        assert_eq!(update.state, ConnectionState::LimitedConnectivity as i32);
        assert!(update.subscription_update_required);
    }

    #[test]
    fn connection_feedback_coalesces_pressure_and_reports_recovery() {
        let mut media = ApiMediaRuntime::default();
        let now = Instant::now();
        media.bandwidth.transport_estimate_bps = Some(2_000_000);
        media.report_connection(now, &demand(4_000_000, 500_000));
        assert_eq!(
            update(&media).state,
            ConnectionState::LimitedConnectivity as i32
        );
        assert!(!update(&media).subscription_update_required);
        media.bandwidth.transport_estimate_bps = Some(6_000_000);
        media.report_connection(
            now + Duration::from_millis(100),
            &demand(4_000_000, 500_000),
        );
        assert_eq!(media.control_notifications.len(), 1);
        media.report_connection(now + Duration::from_secs(1), &demand(4_000_000, 500_000));
        assert_eq!(update(&media).state, ConnectionState::Healthy as i32);
        assert_eq!(update(&media).available_bitrate_bps, 6_000_000);
        assert!(!update(&media).subscription_update_required);
        media.report_connection(now + Duration::from_secs(2), &demand(4_000_000, 500_000));
        assert_eq!(media.control_notifications.len(), 2);
    }

    #[test]
    fn connection_feedback_reports_unsatisfied_manual_demand() {
        let mut media = ApiMediaRuntime::default();
        media.bandwidth.transport_estimate_bps = Some(2_000_000);
        media.report_connection(Instant::now(), &demand(500_000, 4_000_000));
        assert_eq!(
            update(&media).state,
            ConnectionState::LimitedConnectivity as i32
        );
        assert!(update(&media).subscription_update_required);
    }

    #[test]
    fn connection_feedback_respects_queue_bound_and_retries_latest_snapshot() {
        let mut media = ApiMediaRuntime::default();
        let now = Instant::now();
        for _ in 0..API_CONTROL_NOTIFICATION_MAX_MESSAGES {
            media
                .enqueue_control_notification(Notification::default())
                .unwrap();
        }
        media.bandwidth.transport_estimate_bps = Some(1_000_000);
        media.report_connection(now, &demand(4_000_000, 500_000));
        assert_eq!(
            media.control_notifications.len(),
            API_CONTROL_NOTIFICATION_MAX_MESSAGES
        );
        media.flush_control_notifications(|_| Ok(true)).unwrap();
        media.bandwidth.transport_estimate_bps = Some(6_000_000);
        media.report_connection(now + Duration::from_secs(1), &demand(4_000_000, 500_000));
        assert_eq!(media.control_notifications.len(), 1);
        assert_eq!(update(&media).available_bitrate_bps, 6_000_000);
        assert_eq!(update(&media).state, ConnectionState::Healthy as i32);
    }
}
