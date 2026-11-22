use chrono::{DateTime, Datelike, Duration, NaiveDateTime, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::{Arc, Mutex, RwLock},
};

const MAX_WINDOWS: usize = 64;
pub(crate) const MAX_GUEST_MEDIA_TRANSFERS: usize = 8;
const MAX_PROFILE_OVERRIDES_DURING_TRANSITION: usize = crate::profiles::MAX_PROFILE_TARGETS * 2;

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
    responses: Mutex<Vec<(Arc<str>, rouille::ResponseAbortHandle)>>,
}

#[derive(Debug, Default)]
struct PrivacyState {
    schedules: BTreeMap<String, PrivacySchedule>,
    profile_overrides: BTreeMap<String, bool>,
    gates: BTreeMap<String, std::sync::Arc<PrivacyGate>>,
}

impl PrivacyState {
    fn active(&self, id: &str, instant: DateTime<Utc>) -> anyhow::Result<bool> {
        Ok(self.profile_overrides.get(id).copied().unwrap_or(false)
            || self
                .schedules
                .get(id)
                .map(|schedule| schedule.is_active_unchecked(instant))
                .transpose()?
                .unwrap_or(false))
    }
}

/// Captures permission at media ingress and rechecks it at the final local write.
#[derive(Clone, Debug)]
pub struct MediaPermit {
    registry: Arc<PrivacyRegistry>,
    camera_id: Arc<str>,
    gate: Arc<PrivacyGate>,
    epoch: u64,
    alias_generation: u64,
}

impl MediaPermit {
    fn capture(
        registry: &Arc<PrivacyRegistry>,
        state: &PrivacyState,
        id: &str,
        camera_id: &Arc<str>,
        alias_generation: u64,
    ) -> Option<Self> {
        let gate = state.gates.get(id)?;
        let active = state.active(id, Utc::now()).unwrap_or(true);
        let epoch = gate.set_active(active);
        (!active).then(|| Self {
            registry: registry.clone(),
            camera_id: camera_id.clone(),
            gate: gate.clone(),
            epoch,
            alias_generation,
        })
    }

    /// Arms the existing bounded guest HTTP writer for synchronous cancellation at an epoch change.
    pub(crate) fn protect_response(
        &self,
        response: rouille::ResponseAbortHandle,
    ) -> std::io::Result<()> {
        self.deliver(|| {
            let mut responses = self
                .gate
                .responses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            responses.retain(|(_, r)| !r.is_finished().unwrap_or(false));
            if responses.len() >= MAX_GUEST_MEDIA_TRANSFERS {
                return Err(std::io::Error::other(
                    "guest response privacy capacity reached",
                ));
            }
            responses.push((self.camera_id.clone(), response));
            Ok(())
        })
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "camera privacy changed",
            )
        })?
    }

    pub(crate) fn deliver<T>(&self, write: impl FnOnce() -> T) -> Option<T> {
        let aliases = self
            .registry
            .aliases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (id, generation) = resolve_alias(&aliases, &self.camera_id);
        if generation != self.alias_generation {
            return None;
        }
        let state = self
            .registry
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let gate = state.gates.get(id)?;
        if !Arc::ptr_eq(gate, &self.gate) {
            return None;
        }
        let _guard = gate
            .transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active = state.active(id, Utc::now()).unwrap_or(true);
        let epoch = gate.set_active_locked(active);
        (!active && epoch == self.epoch).then(write)
    }
}

/// Remembers every camera epoch admitted to one multiplexed RTC transport.
/// The encrypted datagrams and SCTP retransmissions no longer expose a camera ID.
#[derive(Default)]
pub(crate) struct TransportPermits {
    permits: Vec<MediaPermit>,
}

impl TransportPermits {
    pub(crate) fn remember(&mut self, permit: Option<&MediaPermit>) -> anyhow::Result<()> {
        let Some(permit) = permit else {
            return Ok(());
        };
        if let Some(first) = self.permits.first() {
            anyhow::ensure!(
                Arc::ptr_eq(&first.registry.state, &permit.registry.state)
                    && Arc::ptr_eq(&first.registry.aliases, &permit.registry.aliases),
                "RTC media privacy registry changed"
            );
        }
        // A stable gate order also orders concurrent sends for two shared cameras.
        let key = (Arc::as_ptr(&permit.gate), permit.camera_id.as_ref());
        match self
            .permits
            .binary_search_by(|p| (Arc::as_ptr(&p.gate), p.camera_id.as_ref()).cmp(&key))
        {
            Ok(i) => anyhow::ensure!(
                self.permits[i].epoch == permit.epoch
                    && self.permits[i].alias_generation == permit.alias_generation,
                "RTC media privacy epoch changed"
            ),
            Err(i) => {
                anyhow::ensure!(
                    self.permits.len() < 128,
                    "RTC camera history requires a fresh transport"
                );
                self.permits.insert(i, permit.clone());
            }
        }
        Ok(())
    }

    /// Serialize the nonblocking UDP send with every applicable privacy transition.
    /// A revoked permit permanently retires its peer, including after privacy ends.
    pub(crate) fn send<T>(&self, write: impl FnOnce() -> std::io::Result<T>) -> anyhow::Result<T> {
        let Some(first) = self.permits.first() else {
            return Ok(write()?);
        };
        let aliases = first
            .registry
            .aliases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = first
            .registry
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lock_transport_gates(&self.permits, || {
            let now = Utc::now();
            for permit in &self.permits {
                let (id, generation) = resolve_alias(&aliases, &permit.camera_id);
                anyhow::ensure!(
                    generation == permit.alias_generation,
                    "RTC media privacy identity changed"
                );
                anyhow::ensure!(
                    state
                        .gates
                        .get(id)
                        .is_some_and(|gate| Arc::ptr_eq(gate, &permit.gate)),
                    "RTC media privacy identity changed"
                );
                let active = state.active(id, now).unwrap_or(true);
                let epoch = permit.gate.set_active_locked(active);
                anyhow::ensure!(
                    !active && epoch == permit.epoch,
                    "RTC media privacy epoch changed"
                );
            }
            Ok(write()?)
        })
    }
}

// The 128-entry history bounds recursion. Aliases for one gate share a single lock.
fn lock_transport_gates<T>(permits: &[MediaPermit], write: impl FnOnce() -> T) -> T {
    let Some(first) = permits.first() else {
        return write();
    };
    let _guard = first
        .gate
        .transition
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let next = permits.partition_point(|permit| Arc::ptr_eq(&permit.gate, &first.gate));
    lock_transport_gates(&permits[next..], write)
}

#[derive(Clone, Debug)]
struct CameraAlias {
    camera_id: String,
    generation: u64,
}

fn resolve_alias<'a>(
    aliases: &'a BTreeMap<String, CameraAlias>,
    camera: &'a str,
) -> (&'a str, u64) {
    aliases.get(camera).map_or((camera, 0), |alias| {
        (alias.camera_id.as_str(), alias.generation)
    })
}

/// Resolves the effective schedule for each camera.
#[derive(Debug, Clone, Default)]
pub struct PrivacyRegistry {
    state: Arc<RwLock<PrivacyState>>,
    aliases: Arc<RwLock<BTreeMap<String, CameraAlias>>>,
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
            state: Arc::new(RwLock::new(PrivacyState {
                schedules,
                profile_overrides: BTreeMap::new(),
                gates,
            })),
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
        state.schedules = replacement_state.schedules.clone();
        let PrivacyState {
            schedules,
            profile_overrides,
            gates,
        } = &mut *state;
        gates.retain(|id, gate| {
            schedules.contains_key(id)
                || profile_overrides.contains_key(id)
                || Arc::strong_count(gate) > 1
                || gate.has_responses()
        });
        for id in schedules.keys().chain(profile_overrides.keys()) {
            gates
                .entry(id.clone())
                .or_insert_with(|| Arc::new(PrivacyGate::new()));
        }
        let now = Utc::now();
        for (id, gate) in &state.gates {
            gate.set_active(state.active(id, now).unwrap_or(true));
        }
        Ok(())
    }

    /// Replaces profile privacy inputs; false values never weaken a schedule.
    /// The bound holds both the old and new profile targets during a transition.
    pub fn replace_profile_overrides(
        &self,
        overrides: BTreeMap<String, bool>,
    ) -> anyhow::Result<()> {
        if overrides.len() > MAX_PROFILE_OVERRIDES_DURING_TRANSITION {
            anyhow::bail!(
                "profile privacy overrides cannot contain more than {MAX_PROFILE_OVERRIDES_DURING_TRANSITION} cameras"
            );
        }
        for camera_id in overrides.keys() {
            if camera_id.trim().is_empty() || camera_id.len() > 256 {
                anyhow::bail!("profile privacy camera keys must contain 1 to 256 bytes");
            }
        }
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Utc::now();
        let affected = state
            .profile_overrides
            .keys()
            .chain(overrides.keys())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for camera_id in &affected {
            state
                .gates
                .entry(camera_id.clone())
                .or_insert_with(|| Arc::new(PrivacyGate::new()));
        }
        for camera_id in affected {
            let profile_active = overrides.get(&camera_id).copied().unwrap_or(false);
            let scheduled_active = state
                .schedules
                .get(&camera_id)
                .map(|schedule| schedule.is_active_unchecked(now))
                .transpose()
                .map(|scheduled| scheduled.unwrap_or(false))
                .unwrap_or(true);
            if let Some(gate) = state.gates.get(&camera_id) {
                gate.set_active(profile_active || scheduled_active);
            }
        }
        state.profile_overrides = overrides;
        Ok(())
    }

    pub(crate) fn profile_overrides_snapshot(&self) -> BTreeMap<String, bool> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .profile_overrides
            .clone()
    }

    /// Associates a transport identity with a configured camera identity.
    pub fn set_alias(&self, alias: String, camera_id: String) {
        let mut aliases = self
            .aliases
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (previous, generation) = resolve_alias(&aliases, &alias);
        if previous == camera_id {
            return;
        }
        if let Some(gate) = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .gates
            .get(previous)
        {
            gate.abort_responses(Some(&alias));
        }
        aliases.insert(
            alias,
            CameraAlias {
                camera_id,
                generation: generation.wrapping_add(1),
            },
        );
    }

    /// Returns the effective active state and transition epoch for a camera.
    pub fn decision(&self, camera_id: &str, instant: DateTime<Utc>) -> anyhow::Result<(bool, u64)> {
        let configured_id = self
            .aliases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(camera_id)
            .map_or_else(|| camera_id.to_owned(), |alias| alias.camera_id.clone());
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active = state.active(&configured_id, instant)?;
        if !active && !state.gates.contains_key(&configured_id) {
            return Ok((false, 0));
        }
        let gate = state
            .gates
            .get(&configured_id)
            .ok_or_else(|| anyhow::anyhow!("privacy gate is missing for configured camera"))?;
        let epoch = gate.set_active(active);
        Ok((active, epoch))
    }

    pub(crate) fn media_permit(self: &Arc<Self>, camera_id: Arc<str>) -> Option<MediaPermit> {
        let aliases = self
            .aliases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (id, alias_generation) = resolve_alias(&aliases, &camera_id);
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.gates.contains_key(id) {
            drop(state);
            let mut state = self
                .state
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.gates.len() >= MAX_PROFILE_OVERRIDES_DURING_TRANSITION {
                let PrivacyState {
                    gates,
                    schedules,
                    profile_overrides,
                } = &mut *state;
                gates.retain(|key, gate| {
                    schedules.contains_key(key)
                        || profile_overrides.contains_key(key)
                        || Arc::strong_count(gate) > 1
                        || gate.has_responses()
                });
                if state.gates.len() >= MAX_PROFILE_OVERRIDES_DURING_TRANSITION
                    && !state.gates.contains_key(id)
                {
                    return None;
                }
            }
            state
                .gates
                .entry(id.to_owned())
                .or_insert_with(|| Arc::new(PrivacyGate::new()));
        } else {
            return MediaPermit::capture(self, &state, id, &camera_id, alias_generation);
        }
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        MediaPermit::capture(self, &state, id, &camera_id, alias_generation)
    }

    /// Returns the gate for a configured camera.
    pub fn gate(&self, camera_id: &str) -> Option<std::sync::Arc<PrivacyGate>> {
        let configured_id = self
            .aliases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(camera_id)
            .map_or_else(|| camera_id.to_owned(), |alias| alias.camera_id.clone());
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
            .map_or_else(|| camera_id.to_owned(), |alias| alias.camera_id.clone());
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
            responses: Mutex::new(Vec::new()),
        }
    }

    fn has_responses(&self) -> bool {
        let mut responses = self
            .responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        responses.retain(|(_, r)| !r.is_finished().unwrap_or(false));
        !responses.is_empty()
    }

    fn abort_responses(&self, alias: Option<&str>) {
        self.responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(camera, response)| {
                if alias.is_some_and(|name| name != camera.as_ref()) {
                    return true;
                }
                if response.abort().is_err() {
                    tracing::warn!("guest media transport cancellation failed");
                }
                false
            });
    }

    fn set_active(&self, active: bool) -> u64 {
        let _transition = self
            .transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.set_active_locked(active)
    }

    fn set_active_locked(&self, active: bool) -> u64 {
        if self.active.load(Ordering::Acquire) == active {
            return self.epoch();
        }
        self.abort_responses(None);
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

    #[test]
    fn profile_privacy_activates_the_existing_media_gate_and_can_be_cleared() {
        let registry = PrivacyRegistry::default();
        registry
            .replace_profile_overrides(BTreeMap::from([("front".to_owned(), true)]))
            .unwrap();
        let now = Utc::now();
        let gate = registry.gate("front").unwrap();
        assert!(!gate.allows(gate.epoch()));
        let (active, epoch) = registry.decision("front", now).unwrap();
        assert!(active);
        assert!(!gate.allows(epoch));

        registry
            .replace_profile_overrides(BTreeMap::from([("front".to_owned(), false)]))
            .unwrap();
        assert!(gate.allows(gate.epoch()));
        let (active, epoch) = registry.decision("front", now).unwrap();
        assert!(!active);
        assert!(gate.allows(epoch));
    }

    #[test]
    fn profile_privacy_cannot_weaken_scheduled_privacy() {
        let mut schedules = BTreeMap::new();
        schedules.insert(
            "front".to_owned(),
            schedule(PrivacyWindow {
                weekdays: vec![1],
                start: "00:00".into(),
                end: "23:59".into(),
            }),
        );
        let registry = PrivacyRegistry::new(schedules).unwrap();
        registry
            .replace_profile_overrides(BTreeMap::from([("front".to_owned(), false)]))
            .unwrap();

        let (active, epoch) = registry
            .decision("front", "2026-09-22T05:15:00Z".parse().unwrap())
            .unwrap();

        assert!(active);
        assert!(!registry.gate("front").unwrap().allows(epoch));
    }

    #[test]
    fn profile_privacy_holds_two_bounded_generations_without_weakening_schedule() {
        let registry = PrivacyRegistry::new(BTreeMap::from([(
            "scheduled".to_owned(),
            schedule(PrivacyWindow {
                weekdays: vec![1],
                start: "00:00".into(),
                end: "23:59".into(),
            }),
        )]))
        .unwrap();
        let mut overrides = (0..253)
            .map(|index| (format!("camera-{index}"), true))
            .collect::<BTreeMap<_, _>>();
        overrides.insert("scheduled".to_owned(), false);
        registry
            .replace_profile_overrides(overrides.clone())
            .unwrap();
        assert_eq!(registry.profile_overrides_snapshot(), overrides);
        assert!(
            registry
                .decision("scheduled", "2026-09-22T05:15:00Z".parse().unwrap())
                .unwrap()
                .0
        );

        let mut excessive = overrides.clone();
        excessive.insert("camera-254".to_owned(), true);
        assert!(registry.replace_profile_overrides(excessive).is_err());
        assert_eq!(registry.profile_overrides_snapshot(), overrides);
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
    fn vancouver_privacy_window_keeps_utc_minus_seven_in_november_2026() {
        let mut policy = schedule(PrivacyWindow {
            weekdays: vec![1],
            start: "09:00".into(),
            end: "10:00".into(),
        });
        policy.timezone = "America/Vancouver".into();
        let before = "2026-11-02T15:59:59Z".parse().unwrap();
        let start = "2026-11-02T16:00:00Z".parse().unwrap();
        let end = "2026-11-02T17:00:00Z".parse().unwrap();
        assert_eq!(policy.next_transition(before).unwrap(), Some(start));
        assert!(policy.is_active(start).unwrap());
        assert_eq!(policy.next_transition(start).unwrap(), Some(end));
        assert!(!policy.is_active(end).unwrap());
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
    fn scheduled_privacy_revokes_audio_queued_across_two_configuration_changes() {
        let registry = Arc::new(PrivacyRegistry::default());
        registry.set_alias("transport".into(), "front".into());
        let old = registry.media_permit("transport".into()).unwrap();
        let other = registry.media_permit("back".into()).unwrap();
        let mut full_day = schedule(PrivacyWindow {
            weekdays: (1..=7).collect(),
            start: "00:00".into(),
            end: "12:00".into(),
        });
        full_day.windows.push(PrivacyWindow {
            weekdays: (1..=7).collect(),
            start: "12:00".into(),
            end: "00:00".into(),
        });
        registry
            .replace_schedules([("front".into(), full_day)].into())
            .unwrap();
        assert!(registry.media_permit("transport".into()).is_none());
        registry.replace_schedules(BTreeMap::new()).unwrap();
        assert!(old.deliver(|| true).is_none());
        assert_eq!(other.deliver(|| true), Some(true));
        let fresh = registry.media_permit("transport".into()).unwrap();
        assert_eq!(fresh.deliver(|| true), Some(true));
        registry.set_alias("transport".into(), "back".into());
        assert!(fresh.deliver(|| true).is_none());
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

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    fn restored_alias_cannot_resurrect_old_protected_output() {
        let registry = Arc::new(PrivacyRegistry::default());
        registry.set_alias("input".into(), "front".into());
        let old = registry.media_permit("input".into()).unwrap();
        let mut permits = TransportPermits::default();
        permits.remember(Some(&old)).unwrap();
        registry.set_alias("input".into(), "back".into());
        registry.set_alias("input".into(), "front".into());
        assert!(old.deliver(|| ()).is_none());
        assert!(permits.send(|| Ok(())).is_err());
        let fresh = registry.media_permit("input".into()).unwrap();
        assert!(permits.remember(Some(&fresh)).is_err());
        assert_eq!(fresh.deliver(|| true), Some(true));
    }
}
