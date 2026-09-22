use chrono::{DateTime, Datelike, Duration, NaiveDateTime, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::{Arc, Mutex, RwLock},
};

const MAX_WINDOWS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacySource {
    Default,
    Camera,
}

/// A cheap, shared gate for media producers and consumers.
#[derive(Debug, Default)]
pub struct PrivacyGate {
    active: AtomicBool,
    epoch: AtomicU64,
    transition: Mutex<()>,
}

#[derive(Debug, Default)]
struct PrivacyState {
    schedules: BTreeMap<String, PrivacySchedule>,
    gates: BTreeMap<String, std::sync::Arc<PrivacyGate>>,
}

/// Resolves the effective schedule for each camera.
#[derive(Debug, Clone, Default)]
pub struct PrivacyRegistry {
    state: Arc<RwLock<PrivacyState>>,
    aliases: Arc<RwLock<BTreeMap<String, String>>>,
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
        Ok(Self {
            state: Arc::new(RwLock::new(PrivacyState { schedules, gates })),
            aliases: Arc::new(RwLock::new(BTreeMap::new())),
        })
    }

    /// Replaces validated schedules without replacing the shared registry handle.
    pub fn replace_schedules(
        &self,
        schedules: BTreeMap<String, PrivacySchedule>,
    ) -> anyhow::Result<()> {
        let replacement = Self::new(schedules)?;
        let replacement_state = replacement
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let old_gates = std::mem::take(&mut state.gates);
        state.gates = replacement_state
            .schedules
            .keys()
            .map(|camera_id| {
                (
                    camera_id.clone(),
                    old_gates
                        .get(camera_id)
                        .cloned()
                        .unwrap_or_else(|| Arc::new(PrivacyGate::new())),
                )
            })
            .collect();
        state.schedules = replacement_state.schedules.clone();
        Ok(())
    }

    /// Associates a transport identity with a configured camera identity.
    pub fn set_alias(&self, alias: String, camera_id: String) {
        self.aliases
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(alias, camera_id);
    }

    /// Returns the effective active state and transition epoch for a camera.
    pub fn decision(&self, camera_id: &str, instant: DateTime<Utc>) -> anyhow::Result<(bool, u64)> {
        let configured_id = self
            .aliases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(camera_id)
            .cloned()
            .unwrap_or_else(|| camera_id.to_owned());
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(schedule) = state.schedules.get(&configured_id) else {
            return Ok((false, 0));
        };
        let active = schedule.is_active_unchecked(instant)?;
        let gate = state
            .gates
            .get(&configured_id)
            .ok_or_else(|| anyhow::anyhow!("privacy gate is missing for configured camera"))?;
        let epoch = gate.set_active(active);
        Ok((active, epoch))
    }

    /// Returns the gate for a configured camera.
    pub fn gate(&self, camera_id: &str) -> Option<std::sync::Arc<PrivacyGate>> {
        let configured_id = self
            .aliases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(camera_id)
            .cloned()
            .unwrap_or_else(|| camera_id.to_owned());
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .gates
            .get(&configured_id)
            .cloned()
    }

    /// Returns the configured schedule resolved through transport aliases.
    pub fn schedule(&self, camera_id: &str) -> Option<PrivacySchedule> {
        let configured_id = self
            .aliases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(camera_id)
            .cloned()
            .unwrap_or_else(|| camera_id.to_owned());
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .schedules
            .get(&configured_id)
            .cloned()
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
            transition: Mutex::new(()),
        }
    }

    fn set_active(&self, active: bool) -> u64 {
        let _transition = self
            .transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.active.load(Ordering::Acquire) == active {
            return self.epoch();
        }
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        self.active.store(active, Ordering::Release);
        epoch
    }

    /// Activates privacy before publishing the new epoch.
    pub fn activate(&self) -> u64 {
        self.set_active(true)
    }

    /// Deactivates privacy and advances the epoch so stale media is rejected.
    pub fn deactivate(&self) -> u64 {
        self.set_active(false)
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
    /// Whether the schedule can activate privacy. Disabled schedules retain their configuration.
    #[serde(default = "default_privacy_enabled")]
    pub enabled: bool,
    /// IANA timezone name used to interpret the weekly windows.
    pub timezone: String,
    #[serde(default)]
    pub windows: Vec<PrivacyWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporary_override: Option<PrivacyOverride>,
    /// Keeps camera ingress connected while privacy blocks media delivery.
    #[serde(default = "default_keep_camera_connected")]
    pub keep_camera_connected: bool,
}

const fn default_privacy_enabled() -> bool {
    true
}

const fn default_keep_camera_connected() -> bool {
    true
}

/// A bounded, persisted administrator override of a recurring privacy policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyOverride {
    pub actor: String,
    pub reason: String,
    pub accepted_at: String,
    pub expires_at: String,
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
        if let Some(override_) = &self.temporary_override {
            if override_.actor.trim().is_empty() || override_.actor.len() > 256 {
                anyhow::bail!("privacy override actor must contain 1 to 256 bytes");
            }
            if override_.reason.trim().is_empty() || override_.reason.len() > 256 {
                anyhow::bail!("privacy override reason must contain 1 to 256 bytes");
            }
            let accepted_at = parse_override_time(&override_.accepted_at)?;
            let expires_at = parse_override_time(&override_.expires_at)?;
            if expires_at <= accepted_at {
                anyhow::bail!("privacy override expiry must follow acceptance");
            }
            if expires_at - accepted_at > chrono::Duration::hours(24) {
                anyhow::bail!("privacy override cannot exceed 24 hours");
            }
        }
        Ok(())
    }

    /// Returns whether `instant` is inside a privacy window.
    pub fn is_active(&self, instant: DateTime<Utc>) -> anyhow::Result<bool> {
        self.validate()?;
        self.is_active_unchecked(instant)
    }

    fn is_active_unchecked(&self, instant: DateTime<Utc>) -> anyhow::Result<bool> {
        if !self.enabled {
            return Ok(false);
        }
        if self.temporary_override.as_ref().is_some_and(|override_| {
            let Ok(accepted_at) = parse_override_time(&override_.accepted_at) else {
                return true;
            };
            let Ok(expires_at) = parse_override_time(&override_.expires_at) else {
                return true;
            };
            instant >= accepted_at && instant < expires_at
        }) {
            return Ok(false);
        }
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
                if window.weekdays.contains(&previous_weekday) && local_time < end {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Returns the next effective state transition within the next eight days.
    pub fn next_transition(&self, instant: DateTime<Utc>) -> anyhow::Result<Option<DateTime<Utc>>> {
        self.validate()?;
        if !self.enabled {
            return Ok(None);
        }
        let zone: Tz = self
            .timezone
            .parse()
            .map_err(|_| anyhow::anyhow!("privacy schedule timezone must be a valid IANA zone"))?;
        let local = instant.with_timezone(&zone).date_naive();
        let mut candidates = Vec::new();
        for offset in 0..=8 {
            let date = local + Duration::days(offset);
            let weekday = weekday_number(date.weekday());
            for window in &self.windows {
                if !window.weekdays.contains(&weekday) {
                    continue;
                }
                let start = parse_time(&window.start)?;
                let end = parse_time(&window.end)?;
                for (time, date) in [
                    (start, date),
                    (
                        end,
                        if start > end {
                            date + Duration::days(1)
                        } else {
                            date
                        },
                    ),
                ] {
                    let local_time = date.and_time(time);
                    match zone.from_local_datetime(&local_time) {
                        chrono::LocalResult::Single(value) => {
                            candidates.push(value.with_timezone(&Utc));
                        }
                        chrono::LocalResult::Ambiguous(first, second) => {
                            candidates.push(first.with_timezone(&Utc));
                            candidates.push(second.with_timezone(&Utc));
                        }
                        chrono::LocalResult::None => {
                            if let Some(value) = first_utc_at_or_after_local(&zone, local_time) {
                                candidates.push(value);
                            }
                        }
                    }
                }
            }
        }
        if let Some(override_) = &self.temporary_override {
            candidates.push(parse_override_time(&override_.accepted_at)?);
            candidates.push(parse_override_time(&override_.expires_at)?);
        }
        candidates.sort_unstable();
        candidates.dedup();
        let before = self.is_active_unchecked(instant)?;
        Ok(candidates.into_iter().find(|candidate| {
            *candidate > instant
                && self
                    .is_active_unchecked(*candidate + Duration::milliseconds(1))
                    .ok()
                    .is_some_and(|after| after != before)
        }))
    }
}

fn first_utc_at_or_after_local(zone: &Tz, target: NaiveDateTime) -> Option<DateTime<Utc>> {
    let guess = DateTime::<Utc>::from_naive_utc_and_offset(target, Utc);
    for minute in -2_880..=2_880 {
        let candidate = guess + Duration::minutes(minute);
        if candidate.with_timezone(zone).naive_local() >= target {
            return Some(candidate);
        }
    }
    None
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

fn parse_override_time(value: &str) -> anyhow::Result<DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|_| anyhow::anyhow!("privacy override timestamps must use RFC3339"))
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
            enabled: true,
            timezone: "America/Los_Angeles".into(),
            windows: vec![window],
            temporary_override: None,
            keep_camera_connected: true,
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
        assert!(
            !policy
                .is_active("2026-09-23T06:00:00Z".parse().unwrap())
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
                enabled: true,
                timezone: "UTC-8".into(),
                windows: vec![],
                temporary_override: None,
                keep_camera_connected: true,
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
                enabled: true,
                timezone: "UTC".into(),
                windows,
                temporary_override: None,
                keep_camera_connected: true,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn bounded_override_is_restart_safe_and_expires() {
        let mut policy = schedule(PrivacyWindow {
            weekdays: vec![1],
            start: "00:00".into(),
            end: "23:59".into(),
        });
        policy.temporary_override = Some(PrivacyOverride {
            actor: "admin".into(),
            reason: "maintenance".into(),
            accepted_at: "2026-09-21T01:00:00Z".into(),
            expires_at: "2026-09-21T02:00:00Z".into(),
        });
        policy.timezone = "UTC".into();
        assert!(
            !policy
                .is_active("2026-09-21T01:30:00Z".parse().unwrap())
                .unwrap()
        );
        assert!(
            policy
                .is_active("2026-09-21T02:00:00Z".parse().unwrap())
                .unwrap()
        );
    }

    #[test]
    fn reports_next_transition_for_overnight_policy() {
        let policy = schedule(PrivacyWindow {
            weekdays: vec![1],
            start: "22:00".into(),
            end: "06:00".into(),
        });
        let next = policy
            .next_transition("2026-09-21T20:00:00Z".parse().unwrap())
            .unwrap();
        assert_eq!(next, Some("2026-09-22T05:00:00Z".parse().unwrap()));
    }

    #[test]
    fn skips_nonexistent_spring_forward_local_times() {
        let policy = PrivacySchedule {
            enabled: true,
            timezone: "America/Los_Angeles".into(),
            windows: vec![PrivacyWindow {
                weekdays: vec![7],
                start: "02:30".into(),
                end: "03:30".into(),
            }],
            temporary_override: None,
            keep_camera_connected: true,
        };
        let next = policy
            .next_transition("2026-03-08T09:00:00Z".parse().unwrap())
            .unwrap();
        assert_eq!(next, Some("2026-03-08T10:00:00Z".parse().unwrap()));
    }

    #[test]
    fn disabled_schedule_never_transitions_or_activates() {
        let mut policy = schedule(PrivacyWindow {
            weekdays: vec![1],
            start: "00:00".into(),
            end: "23:59".into(),
        });
        policy.enabled = false;
        let instant = "2026-09-21T12:00:00Z".parse().unwrap();
        assert!(!policy.is_active(instant).unwrap());
        assert_eq!(policy.next_transition(instant).unwrap(), None);
    }

    #[test]
    fn replacing_schedules_preserves_aliases_and_gate_epochs() {
        let mut schedules = BTreeMap::new();
        schedules.insert(
            "front".to_owned(),
            schedule(PrivacyWindow {
                weekdays: vec![1],
                start: "00:00".into(),
                end: "01:00".into(),
            }),
        );
        let registry = PrivacyRegistry::new(schedules).unwrap();
        registry.set_alias("192.0.2.10".into(), "front".into());
        let before = registry
            .decision("192.0.2.10", "2026-09-21T07:30:00Z".parse().unwrap())
            .unwrap();
        registry.replace_schedules(BTreeMap::new()).unwrap();
        assert_eq!(
            registry.decision("192.0.2.10", Utc::now()).unwrap(),
            (false, 0)
        );
        registry
            .replace_schedules({
                let mut replacement = BTreeMap::new();
                replacement.insert(
                    "front".to_owned(),
                    schedule(PrivacyWindow {
                        weekdays: vec![1],
                        start: "00:00".into(),
                        end: "01:00".into(),
                    }),
                );
                replacement
            })
            .unwrap();
        let after = registry
            .decision("192.0.2.10", "2026-09-21T07:30:00Z".parse().unwrap())
            .unwrap();
        assert_eq!(before.0, after.0);
        assert!(after.1 >= before.1);
    }

    #[test]
    fn replacing_schedules_is_atomic_for_concurrent_decisions() {
        let mut schedules = BTreeMap::new();
        schedules.insert(
            "front".to_owned(),
            schedule(PrivacyWindow {
                weekdays: vec![1],
                start: "00:00".into(),
                end: "01:00".into(),
            }),
        );
        let registry = Arc::new(PrivacyRegistry::new(schedules).unwrap());
        let workers = (0..4)
            .map(|_| {
                let registry = registry.clone();
                std::thread::spawn(move || {
                    for _ in 0..256 {
                        registry.decision("front", Utc::now()).unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();

        for iteration in 0..256 {
            let schedules = if iteration % 2 == 0 {
                let mut schedules = BTreeMap::new();
                schedules.insert(
                    "front".to_owned(),
                    schedule(PrivacyWindow {
                        weekdays: vec![1],
                        start: "00:00".into(),
                        end: "01:00".into(),
                    }),
                );
                schedules
            } else {
                BTreeMap::new()
            };
            registry.replace_schedules(schedules).unwrap();
        }
        for worker in workers {
            worker.join().unwrap();
        }
    }
}
