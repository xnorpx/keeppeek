use chrono::{DateTime, Datelike, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

const MAX_WINDOWS: usize = 64;

/// A cheap, shared gate for media producers and consumers.
#[derive(Debug, Default)]
pub struct PrivacyGate {
    active: AtomicBool,
    epoch: AtomicU64,
}

/// Resolves the effective schedule for each camera.
#[derive(Debug, Clone, Default)]
pub struct PrivacyRegistry {
    schedules: BTreeMap<String, PrivacySchedule>,
    gates: BTreeMap<String, std::sync::Arc<PrivacyGate>>,
}

impl PrivacyRegistry {
    /// Builds a registry from validated configuration.
    pub fn new(schedules: BTreeMap<String, PrivacySchedule>) -> anyhow::Result<Self> {
        if schedules.len() > 127 {
            anyhow::bail!("privacy registry cannot contain more than 127 cameras");
        }
        for (camera_id, schedule) in &schedules {
            if camera_id.trim().is_empty() || camera_id.len() > 256 {
                anyhow::bail!("privacy camera keys must contain 1 to 256 bytes");
            }
            schedule.validate()?;
        }
        let gates = schedules
            .keys()
            .map(|camera_id| (camera_id.clone(), std::sync::Arc::new(PrivacyGate::new())))
            .collect();
        Ok(Self { schedules, gates })
    }

    /// Returns the effective active state and transition epoch for a camera.
    pub fn decision(&self, camera_id: &str, instant: DateTime<Utc>) -> anyhow::Result<(bool, u64)> {
        let Some(schedule) = self.schedules.get(camera_id) else {
            return Ok((false, 0));
        };
        let active = schedule.is_active(instant)?;
        let gate = self
            .gates
            .get(camera_id)
            .expect("configured privacy gate exists");
        let epoch = if active == gate.is_active() {
            gate.epoch()
        } else if active {
            gate.activate()
        } else {
            gate.deactivate()
        };
        Ok((active, epoch))
    }

    /// Returns the gate for a configured camera.
    pub fn gate(&self, camera_id: &str) -> Option<std::sync::Arc<PrivacyGate>> {
        self.gates.get(camera_id).cloned()
    }

    /// Resolves a camera whose configured identifier is its transport address.
    pub fn decision_for_ip(
        &self,
        camera_ip: std::net::IpAddr,
        instant: DateTime<Utc>,
    ) -> anyhow::Result<(bool, u64)> {
        self.decision(&camera_ip.to_string(), instant)
    }
}

impl PrivacyGate {
    /// Creates an inactive gate.
    pub const fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
        }
    }

    /// Activates privacy before publishing the new epoch.
    pub fn activate(&self) -> u64 {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        self.active.store(true, Ordering::Release);
        epoch
    }

    /// Deactivates privacy and advances the epoch so stale media is rejected.
    pub fn deactivate(&self) -> u64 {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        self.active.store(false, Ordering::Release);
        epoch
    }

    /// Returns whether a producer may publish media for the observed epoch.
    pub fn allows(&self, observed_epoch: u64) -> bool {
        !self.active.load(Ordering::Acquire) && observed_epoch == self.epoch.load(Ordering::Acquire)
    }

    /// Returns the current privacy state.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Returns the current transition epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
}

/// A recurring local-time interval during which a camera is private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyWindow {
    /// ISO weekday numbers, where Monday is 1 and Sunday is 7.
    pub weekdays: Vec<u8>,
    /// Inclusive local start in `HH:MM` form.
    pub start: String,
    /// Exclusive local end in `HH:MM` form.
    pub end: String,
}

/// A validated recurring privacy policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacySchedule {
    /// IANA timezone name used to interpret the weekly windows.
    pub timezone: String,
    #[serde(default)]
    pub windows: Vec<PrivacyWindow>,
}

impl PrivacySchedule {
    /// Validates the schedule without consulting the system timezone.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.windows.len() > MAX_WINDOWS {
            anyhow::bail!("privacy schedule cannot contain more than {MAX_WINDOWS} windows");
        }
        let _: Tz = self
            .timezone
            .parse()
            .map_err(|_| anyhow::anyhow!("privacy schedule timezone must be a valid IANA zone"))?;
        for window in &self.windows {
            if window.weekdays.is_empty() {
                anyhow::bail!("privacy schedule windows must include at least one weekday");
            }
            if window.weekdays.iter().any(|day| !(1..=7).contains(day)) {
                anyhow::bail!("privacy schedule weekday must be between 1 and 7");
            }
            let start = parse_time(&window.start)?;
            let end = parse_time(&window.end)?;
            if start == end {
                anyhow::bail!("privacy schedule windows cannot have equal start and end times");
            }
        }
        Ok(())
    }

    /// Returns whether `instant` is inside a privacy window.
    pub fn is_active(&self, instant: DateTime<Utc>) -> anyhow::Result<bool> {
        self.validate()?;
        let zone: Tz = self
            .timezone
            .parse()
            .map_err(|_| anyhow::anyhow!("privacy schedule timezone must be a valid IANA zone"))?;
        let local = instant.with_timezone(&zone);
        let local_time = local.time();
        let weekday = weekday_number(local.weekday());
        for window in &self.windows {
            let start = parse_time(&window.start)?;
            let end = parse_time(&window.end)?;
            if window.weekdays.contains(&weekday) && local_time >= start && local_time < end {
                return Ok(true);
            }
            if start > end && window.weekdays.contains(&weekday) && local_time >= start {
                return Ok(true);
            }
            if start > end {
                let previous_weekday = if weekday == 1 { 7 } else { weekday - 1 };
                if window.weekdays.contains(&previous_weekday)
                    && (local_time >= start || local_time < end)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn parse_time(value: &str) -> anyhow::Result<NaiveTime> {
    if value.len() != 5 || value.as_bytes().get(2) != Some(&b':') {
        anyhow::bail!("privacy schedule time must use HH:MM form");
    }
    let hour = value[..2]
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("privacy schedule hour is invalid"))?;
    let minute = value[3..]
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("privacy schedule minute is invalid"))?;
    NaiveTime::from_hms_opt(hour, minute, 0)
        .ok_or_else(|| anyhow::anyhow!("privacy schedule time is outside the day"))
}

const fn weekday_number(day: Weekday) -> u8 {
    match day {
        Weekday::Mon => 1,
        Weekday::Tue => 2,
        Weekday::Wed => 3,
        Weekday::Thu => 4,
        Weekday::Fri => 5,
        Weekday::Sat => 6,
        Weekday::Sun => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_rejects_stale_and_private_media_epochs() {
        let gate = PrivacyGate::new();
        let initial = gate.epoch();
        assert!(gate.allows(initial));
        let private_epoch = gate.activate();
        assert!(!gate.allows(private_epoch));
        let public_epoch = gate.deactivate();
        assert!(!gate.allows(initial));
        assert!(gate.allows(public_epoch));
    }

    #[test]
    fn registry_changes_epoch_only_at_a_policy_transition() {
        let mut schedules = BTreeMap::new();
        schedules.insert(
            "front".to_owned(),
            schedule(PrivacyWindow {
                weekdays: vec![1],
                start: "22:00".into(),
                end: "23:00".into(),
            }),
        );
        let registry = PrivacyRegistry::new(schedules).unwrap();
        let first = registry
            .decision("front", "2026-09-22T05:15:00Z".parse().unwrap())
            .unwrap();
        let second = registry
            .decision("front", "2026-09-22T05:30:00Z".parse().unwrap())
            .unwrap();
        assert_eq!(first, second);
        assert!(first.0);
    }

    fn schedule(window: PrivacyWindow) -> PrivacySchedule {
        PrivacySchedule {
            timezone: "America/Los_Angeles".into(),
            windows: vec![window],
        }
    }

    #[test]
    fn evaluates_half_open_boundaries_and_overnight_windows() {
        let policy = schedule(PrivacyWindow {
            weekdays: vec![1],
            start: "22:00".into(),
            end: "06:00".into(),
        });
        assert!(
            !policy
                .is_active("2026-09-22T04:59:59Z".parse().unwrap())
                .unwrap()
        );
        assert!(
            policy
                .is_active("2026-09-22T06:00:00Z".parse().unwrap())
                .unwrap()
        );
        assert!(
            policy
                .is_active("2026-09-22T05:00:00Z".parse().unwrap())
                .unwrap()
        );
        assert!(
            !policy
                .is_active("2026-09-22T13:00:00Z".parse().unwrap())
                .unwrap()
        );
    }

    #[test]
    fn protects_both_occurrences_of_a_fall_back_local_time() {
        let policy = schedule(PrivacyWindow {
            weekdays: vec![7],
            start: "01:15".into(),
            end: "01:45".into(),
        });
        assert!(
            policy
                .is_active("2026-11-01T08:30:00Z".parse().unwrap())
                .unwrap()
        );
        assert!(
            policy
                .is_active("2026-11-01T09:30:00Z".parse().unwrap())
                .unwrap()
        );
    }

    #[test]
    fn rejects_invalid_zone_time_and_window_count() {
        assert!(
            PrivacySchedule {
                timezone: "UTC-8".into(),
                windows: vec![]
            }
            .validate()
            .is_err()
        );
        assert!(
            schedule(PrivacyWindow {
                weekdays: vec![1],
                start: "24:00".into(),
                end: "01:00".into()
            })
            .validate()
            .is_err()
        );
        let windows = (0..=MAX_WINDOWS)
            .map(|_| PrivacyWindow {
                weekdays: vec![1],
                start: "00:00".into(),
                end: "00:01".into(),
            })
            .collect();
        assert!(
            PrivacySchedule {
                timezone: "UTC".into(),
                windows
            }
            .validate()
            .is_err()
        );
    }
}
