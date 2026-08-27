use std::{collections::HashSet, str::FromStr};

use chrono::{Datelike, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use url::Url;

use super::{Cooldown, Lifecycle, Stage};

const MAX_ID_BYTES: usize = 128;
const MAX_NAME_CHARS: usize = 128;
const MAX_FILTER_VALUES: usize = 128;
const MAX_ACTIONS: usize = 8;
const MAX_TEMPLATE_TITLE_CHARS: usize = 256;
const MAX_TEMPLATE_BODY_CHARS: usize = 4_096;
const MAX_DESTINATION_BYTES: usize = 2_048;
const MAX_POLICY_DURATION_MS: u64 = 30 * 24 * 60 * 60 * 1_000;
const MAX_ATTACHMENT_BYTES: u64 = 4 * 1_024 * 1_024;
const PUSHOVER_MIN_RETRY_INTERVAL_MS: u64 = 5_000;
const ALLOWED_TEMPLATE_FIELDS: [&str; 10] = [
    "source.id",
    "source.name",
    "event.id",
    "event.kind",
    "event.zone",
    "event.confidence",
    "event.duration",
    "health.state",
    "notification.stage",
    "notification.deep_link",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    EventCreated,
    EventUpdated,
    EventEnded,
    OutageStarted,
    Recovery,
    StorageHealth,
    RecordingHealth,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub trigger: Trigger,
    pub source_id: String,
    pub source_name: Option<String>,
    pub source_identity: String,
    pub lifecycle: Lifecycle,
    pub event_kind: Option<String>,
    pub group_ids: Vec<String>,
    pub zone: Option<String>,
    pub confidence: Option<f64>,
    pub attachment_path: Option<String>,
    pub duration_ms: Option<u64>,
    pub severity: Severity,
    pub reviewed: Option<bool>,
    pub bookmarked: Option<bool>,
    pub privacy_active: bool,
    pub revision: u64,
    pub stage: Stage,
    pub occurred_at_ms: i64,
    pub deep_link: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    const fn number_from_monday(self) -> u32 {
        match self {
            Self::Monday => 1,
            Self::Tuesday => 2,
            Self::Wednesday => 3,
            Self::Thursday => 4,
            Self::Friday => 5,
            Self::Saturday => 6,
            Self::Sunday => 7,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Filter {
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub group_ids: Vec<String>,
    #[serde(default)]
    pub event_kinds: Vec<String>,
    #[serde(default)]
    pub zones: Vec<String>,
    pub minimum_confidence: Option<f64>,
    pub attachment_required: Option<bool>,
    pub minimum_duration_ms: Option<u64>,
    #[serde(default)]
    pub severities: Vec<Severity>,
    pub reviewed: Option<bool>,
    pub bookmarked: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeeklyWindow {
    pub weekdays: Vec<Weekday>,
    pub start_minute: u16,
    pub end_minute: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietHours {
    pub windows: Vec<WeeklyWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub timezone: String,
    #[serde(default)]
    pub active_windows: Vec<WeeklyWindow>,
    pub quiet_hours: Option<QuietHours>,
}

impl Schedule {
    pub fn status_at(&self, timestamp_ms: i64) -> anyhow::Result<ScheduleStatus> {
        let timezone = Tz::from_str(&self.timezone)
            .map_err(|_| anyhow::anyhow!("unknown schedule timezone {}", self.timezone))?;
        let instant = Utc
            .timestamp_millis_opt(timestamp_ms)
            .single()
            .ok_or_else(|| anyhow::anyhow!("schedule timestamp is out of range"))?
            .with_timezone(&timezone);
        let weekday = instant.weekday().number_from_monday();
        let minute = u16::try_from(instant.hour() * 60 + instant.minute())?;
        let active = self.active_windows.is_empty()
            || self
                .active_windows
                .iter()
                .any(|window| window.contains(weekday, minute));
        let quiet = self.quiet_hours.as_ref().is_some_and(|quiet_hours| {
            quiet_hours
                .windows
                .iter()
                .any(|window| window.contains(weekday, minute))
        });
        Ok(ScheduleStatus { active, quiet })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleStatus {
    pub active: bool,
    pub quiet: bool,
}

impl WeeklyWindow {
    fn contains(&self, weekday: u32, minute: u16) -> bool {
        let includes = |day| {
            self.weekdays
                .iter()
                .any(|candidate| candidate.number_from_monday() == day)
        };
        if self.start_minute < self.end_minute {
            includes(weekday) && (self.start_minute..self.end_minute).contains(&minute)
        } else {
            let previous_weekday = if weekday == 1 { 7 } else { weekday - 1 };
            (includes(weekday) && minute >= self.start_minute)
                || (includes(previous_weekday) && minute < self.end_minute)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitScope {
    Rule,
    Channel,
    Principal,
    Global,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    pub scope: RateLimitScope,
    pub maximum: u32,
    pub window_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriticalBypass {
    pub maximum: u32,
    pub window_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Browser,
    Push,
    Webhook,
    Forwarder,
}

impl Channel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Push => "push",
            Self::Webhook => "webhook",
            Self::Forwarder => "forwarder",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentPolicy {
    Never,
    WhenAvailable,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub channel: Channel,
    pub destination: String,
    pub template: Template,
    pub attachment: AttachmentPolicy,
    pub allow_second_delivery: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichmentPolicy {
    pub deadline_ms: u64,
    pub maximum_revisions: u32,
    pub maximum_attempts: u32,
    pub maximum_attachment_bytes: u64,
    pub wake_after_deadline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailurePolicy {
    pub maximum_attempts: u32,
    pub maximum_retry_interval_ms: u64,
    pub expiry_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub revision: u64,
    pub owner_id: String,
    pub triggers: Vec<Trigger>,
    #[serde(default)]
    pub filter: Filter,
    pub schedule: Schedule,
    #[serde(default)]
    pub cooldowns: Vec<Cooldown>,
    #[serde(default)]
    pub rate_limits: Vec<RateLimit>,
    pub critical_bypass: Option<CriticalBypass>,
    pub enrichment: EnrichmentPolicy,
    pub actions: Vec<Action>,
    pub failure: FailurePolicy,
}

impl Rule {
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_id(&self.id, "rule ID")?;
        validate_id(&self.owner_id, "rule owner")?;
        if self.name.trim().is_empty() || self.name.chars().count() > MAX_NAME_CHARS {
            anyhow::bail!("rule name must contain 1 to {MAX_NAME_CHARS} characters");
        }
        if self.triggers.is_empty() {
            anyhow::bail!("a rule must contain at least one trigger");
        }
        ensure_unique(&self.triggers, "rule triggers")?;
        validate_filter(&self.filter)?;
        validate_schedule(&self.schedule)?;
        validate_cooldowns(&self.cooldowns)?;
        validate_rate_limits(&self.rate_limits)?;
        if let Some(bypass) = &self.critical_bypass {
            if bypass.maximum == 0 || bypass.maximum > 10 {
                anyhow::bail!("critical bypass maximum must be between 1 and 10");
            }
            validate_duration(bypass.window_ms, "critical bypass window")?;
        }
        validate_enrichment(&self.enrichment)?;
        validate_failure(&self.failure)?;
        if self.actions.is_empty() || self.actions.len() > MAX_ACTIONS {
            anyhow::bail!("a rule must contain 1 to {MAX_ACTIONS} actions");
        }
        for action in &self.actions {
            validate_action(action)?;
        }
        if self
            .actions
            .iter()
            .any(|action| action.enabled && action.channel == Channel::Push)
            && self.failure.maximum_retry_interval_ms < PUSHOVER_MIN_RETRY_INTERVAL_MS
        {
            anyhow::bail!("Pushover maximum retry interval must be at least 5000 ms");
        }
        Ok(())
    }

    pub fn matches(&self, candidate: &Candidate) -> anyhow::Result<MatchResult> {
        if !self.enabled || !self.triggers.contains(&candidate.trigger) {
            return Ok(MatchResult::NoMatch);
        }
        if !matches_filter(&self.filter, candidate) {
            return Ok(MatchResult::NoMatch);
        }
        let schedule = self.schedule.status_at(candidate.occurred_at_ms)?;
        if !schedule.active {
            return Ok(MatchResult::Suppressed(Suppression::OutsideSchedule));
        }
        if schedule.quiet
            && !(candidate.severity == Severity::Critical && self.critical_bypass.is_some())
        {
            return Ok(MatchResult::Suppressed(Suppression::QuietHours));
        }
        Ok(MatchResult::Match)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suppression {
    OutsideSchedule,
    QuietHours,
}

impl Suppression {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OutsideSchedule => "outside_schedule",
            Self::QuietHours => "quiet_hours",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchResult {
    NoMatch,
    Suppressed(Suppression),
    Match,
}

fn matches_filter(filter: &Filter, candidate: &Candidate) -> bool {
    matches_optional_filter(&filter.source_ids, Some(candidate.source_id.as_str()))
        && (filter.group_ids.is_empty()
            || candidate
                .group_ids
                .iter()
                .any(|group_id| filter.group_ids.contains(group_id)))
        && matches_optional_filter(&filter.event_kinds, candidate.event_kind.as_deref())
        && matches_optional_filter(&filter.zones, candidate.zone.as_deref())
        && filter
            .minimum_confidence
            .is_none_or(|minimum| candidate.confidence.is_some_and(|value| value >= minimum))
        && filter.attachment_required.is_none_or(|required| {
            required == (candidate.attachment_path.is_some() && !candidate.privacy_active)
        })
        && filter
            .minimum_duration_ms
            .is_none_or(|minimum| candidate.duration_ms.is_some_and(|value| value >= minimum))
        && (filter.severities.is_empty() || filter.severities.contains(&candidate.severity))
        && filter
            .reviewed
            .is_none_or(|required| candidate.reviewed == Some(required))
        && filter
            .bookmarked
            .is_none_or(|required| candidate.bookmarked == Some(required))
}

fn matches_optional_filter(values: &[String], candidate: Option<&str>) -> bool {
    values.is_empty()
        || candidate.is_some_and(|candidate| values.iter().any(|value| value == candidate))
}

fn validate_filter(filter: &Filter) -> anyhow::Result<()> {
    for (name, values) in [
        ("source", &filter.source_ids),
        ("group", &filter.group_ids),
        ("event kind", &filter.event_kinds),
        ("zone", &filter.zones),
    ] {
        if values.len() > MAX_FILTER_VALUES {
            anyhow::bail!("{name} filter exceeds {MAX_FILTER_VALUES} values");
        }
        if values.iter().any(|value| value.trim().is_empty()) {
            anyhow::bail!("{name} filter values must not be empty");
        }
    }
    if filter
        .minimum_confidence
        .is_some_and(|confidence| !confidence.is_finite() || !(0.0..=1.0).contains(&confidence))
    {
        anyhow::bail!("minimum confidence must be between 0 and 1");
    }
    Ok(())
}

fn validate_schedule(schedule: &Schedule) -> anyhow::Result<()> {
    Tz::from_str(&schedule.timezone)
        .map_err(|_| anyhow::anyhow!("unknown schedule timezone {}", schedule.timezone))?;
    for window in schedule.active_windows.iter().chain(
        schedule
            .quiet_hours
            .iter()
            .flat_map(|quiet_hours| quiet_hours.windows.iter()),
    ) {
        if window.weekdays.is_empty() {
            anyhow::bail!("schedule windows require at least one weekday");
        }
        ensure_unique(&window.weekdays, "schedule weekdays")?;
        if window.start_minute >= 1_440 || window.end_minute >= 1_440 {
            anyhow::bail!("schedule minutes must be between 0 and 1439");
        }
        if window.start_minute == window.end_minute {
            anyhow::bail!("schedule window start and end must differ");
        }
    }
    Ok(())
}

fn validate_cooldowns(cooldowns: &[Cooldown]) -> anyhow::Result<()> {
    let mut scopes = HashSet::with_capacity(cooldowns.len());
    for cooldown in cooldowns {
        if !scopes.insert(&cooldown.scope) {
            anyhow::bail!("cooldown scopes must be unique");
        }
        validate_duration(cooldown.duration_ms, "cooldown")?;
    }
    Ok(())
}

fn validate_rate_limits(rate_limits: &[RateLimit]) -> anyhow::Result<()> {
    let mut scopes = HashSet::with_capacity(rate_limits.len());
    for rate_limit in rate_limits {
        if !scopes.insert(rate_limit.scope) {
            anyhow::bail!("rate-limit scopes must be unique");
        }
        if rate_limit.maximum == 0 || rate_limit.maximum > 10_000 {
            anyhow::bail!("rate-limit maximum must be between 1 and 10000");
        }
        validate_duration(rate_limit.window_ms, "rate-limit window")?;
    }
    Ok(())
}

fn validate_enrichment(enrichment: &EnrichmentPolicy) -> anyhow::Result<()> {
    validate_duration(enrichment.deadline_ms, "enrichment deadline")?;
    if enrichment.maximum_revisions == 0 || enrichment.maximum_revisions > 32 {
        anyhow::bail!("maximum enrichment revisions must be between 1 and 32");
    }
    if enrichment.maximum_attempts == 0 || enrichment.maximum_attempts > 8 {
        anyhow::bail!("maximum enrichment attempts must be between 1 and 8");
    }
    if enrichment.maximum_attachment_bytes == 0
        || enrichment.maximum_attachment_bytes > MAX_ATTACHMENT_BYTES
    {
        anyhow::bail!("maximum attachment bytes must be between 1 and {MAX_ATTACHMENT_BYTES}");
    }
    Ok(())
}

fn validate_failure(failure: &FailurePolicy) -> anyhow::Result<()> {
    if failure.maximum_attempts == 0 || failure.maximum_attempts > 10 {
        anyhow::bail!("maximum delivery attempts must be between 1 and 10");
    }
    validate_duration(failure.maximum_retry_interval_ms, "maximum retry interval")?;
    validate_duration(failure.expiry_ms, "outbox expiry")?;
    Ok(())
}

fn validate_action(action: &Action) -> anyhow::Result<()> {
    if action.destination.len() > MAX_DESTINATION_BYTES {
        anyhow::bail!("action destination exceeds {MAX_DESTINATION_BYTES} bytes");
    }
    validate_template(&action.template)?;
    if !action.enabled {
        return Ok(());
    }
    match action.channel {
        Channel::Browser if !action.destination.is_empty() => {
            anyhow::bail!("browser actions use the rule owner and must not set a destination");
        }
        Channel::Webhook => {
            let destination = Url::parse(&action.destination)
                .map_err(|_| anyhow::anyhow!("webhook destination must be an absolute URL"))?;
            if !matches!(destination.scheme(), "http" | "https")
                || !destination.username().is_empty()
                || destination.password().is_some()
            {
                anyhow::bail!("webhook destination must be an HTTP(S) URL without credentials");
            }
        }
        Channel::Push => {
            super::pushover::Destination::parse(&action.destination)?;
            super::pushover::validate_template(&action.template.title, &action.template.body)?;
        }
        Channel::Forwarder if action.destination.trim().is_empty() => {
            anyhow::bail!("{} actions require a destination", action.channel.as_str());
        }
        Channel::Browser | Channel::Forwarder => {}
    }
    Ok(())
}

const fn default_true() -> bool {
    true
}

fn validate_template(template: &Template) -> anyhow::Result<()> {
    if template.title.chars().count() > MAX_TEMPLATE_TITLE_CHARS {
        anyhow::bail!("template title exceeds {MAX_TEMPLATE_TITLE_CHARS} characters");
    }
    if template.body.chars().count() > MAX_TEMPLATE_BODY_CHARS {
        anyhow::bail!("template body exceeds {MAX_TEMPLATE_BODY_CHARS} characters");
    }
    for text in [&template.title, &template.body] {
        let mut remainder = text.as_str();
        while let Some(start) = remainder.find("{{") {
            let after_start = &remainder[start + 2..];
            let Some(end) = after_start.find("}}") else {
                anyhow::bail!("template contains an unterminated field");
            };
            let field = after_start[..end].trim();
            if !ALLOWED_TEMPLATE_FIELDS.contains(&field) {
                anyhow::bail!("template field {field:?} is not allowed");
            }
            remainder = &after_start[end + 2..];
        }
        if remainder.contains("}}") {
            anyhow::bail!("template contains an unmatched closing delimiter");
        }
    }
    Ok(())
}

fn validate_duration(duration_ms: u64, name: &str) -> anyhow::Result<()> {
    if duration_ms == 0 || duration_ms > MAX_POLICY_DURATION_MS {
        anyhow::bail!("{name} must be between 1 ms and {MAX_POLICY_DURATION_MS} ms");
    }
    Ok(())
}

fn validate_id(value: &str, name: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        anyhow::bail!(
            "{name} must contain 1 to {MAX_ID_BYTES} ASCII letters, digits, '.', '-', or '_'"
        );
    }
    Ok(())
}

fn ensure_unique<T>(values: &[T], name: &str) -> anyhow::Result<()>
where
    T: Eq + std::hash::Hash,
{
    let mut seen = HashSet::with_capacity(values.len());
    if values.iter().any(|value| !seen.insert(value)) {
        anyhow::bail!("{name} must be unique");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::CooldownScope;

    fn valid_rule() -> Rule {
        Rule {
            id: "front-door-person".to_owned(),
            name: "Front door person".to_owned(),
            enabled: true,
            revision: 1,
            owner_id: "owner-1".to_owned(),
            triggers: vec![Trigger::EventCreated, Trigger::EventUpdated],
            filter: Filter {
                event_kinds: vec!["person".to_owned()],
                minimum_confidence: Some(0.7),
                ..Filter::default()
            },
            schedule: Schedule {
                timezone: "America/New_York".to_owned(),
                active_windows: Vec::new(),
                quiet_hours: Some(QuietHours {
                    windows: vec![WeeklyWindow {
                        weekdays: vec![Weekday::Sunday],
                        start_minute: 60,
                        end_minute: 180,
                    }],
                }),
            },
            cooldowns: vec![Cooldown {
                scope: CooldownScope::CameraEventKind,
                duration_ms: 30_000,
            }],
            rate_limits: vec![RateLimit {
                scope: RateLimitScope::Rule,
                maximum: 10,
                window_ms: 60_000,
            }],
            critical_bypass: Some(CriticalBypass {
                maximum: 2,
                window_ms: 60_000,
            }),
            enrichment: EnrichmentPolicy {
                deadline_ms: 10_000,
                maximum_revisions: 4,
                maximum_attempts: 2,
                maximum_attachment_bytes: 1_048_576,
                wake_after_deadline: false,
            },
            actions: vec![Action {
                enabled: true,
                channel: Channel::Browser,
                destination: String::new(),
                template: Template {
                    title: "{{event.kind}} at {{source.id}}".to_owned(),
                    body: "Open {{notification.deep_link}}".to_owned(),
                },
                attachment: AttachmentPolicy::WhenAvailable,
                allow_second_delivery: false,
            }],
            failure: FailurePolicy {
                maximum_attempts: 4,
                maximum_retry_interval_ms: 60_000,
                expiry_ms: 3_600_000,
            },
        }
    }

    #[test]
    fn validates_a_bounded_rule_and_rejects_executable_templates() {
        let mut rule = valid_rule();
        rule.validate().unwrap();

        rule.actions[0].template.body = "{{shell.command}}".to_owned();
        assert!(
            rule.validate()
                .unwrap_err()
                .to_string()
                .contains("not allowed")
        );
    }

    #[test]
    fn quiet_hours_follow_timezone_dst_transitions() {
        let schedule = valid_rule().schedule;
        let before_spring_forward = 1_710_052_200_000;
        let after_spring_forward = 1_710_059_400_000;
        assert!(schedule.status_at(before_spring_forward).unwrap().quiet);
        assert!(!schedule.status_at(after_spring_forward).unwrap().quiet);

        let first_fall_hour = 1_730_612_600_000;
        let second_fall_hour = 1_730_616_200_000;
        assert!(schedule.status_at(first_fall_hour).unwrap().quiet);
        assert!(schedule.status_at(second_fall_hour).unwrap().quiet);
    }

    #[test]
    fn overnight_window_uses_the_previous_days_selection() {
        let window = WeeklyWindow {
            weekdays: vec![Weekday::Friday],
            start_minute: 22 * 60,
            end_minute: 6 * 60,
        };
        assert!(window.contains(5, 23 * 60));
        assert!(window.contains(6, 5 * 60));
        assert!(!window.contains(6, 7 * 60));
        assert_ne!(Stage::Preliminary, Stage::Enriched);
    }

    #[test]
    fn complete_candidate_predicates_are_evaluated_without_transport_io() {
        let rule = valid_rule();
        let candidate = Candidate {
            trigger: Trigger::EventCreated,
            source_id: "front-door".to_owned(),
            source_name: Some("Front Door".to_owned()),
            source_identity: "event-1".to_owned(),
            lifecycle: Lifecycle::Event,
            event_kind: Some("person".to_owned()),
            group_ids: Vec::new(),
            zone: None,
            confidence: Some(0.8),
            attachment_path: None,
            duration_ms: None,
            severity: Severity::Info,
            reviewed: Some(false),
            bookmarked: Some(false),
            privacy_active: false,
            revision: 1,
            stage: Stage::Preliminary,
            occurred_at_ms: 1_710_059_400_000,
            deep_link: "/events/event-1".to_owned(),
        };
        assert_eq!(rule.matches(&candidate).unwrap(), MatchResult::Match);

        let mut below_confidence = candidate.clone();
        below_confidence.confidence = Some(0.6);
        assert_eq!(
            rule.matches(&below_confidence).unwrap(),
            MatchResult::NoMatch
        );

        let mut privacy_active = candidate;
        privacy_active.privacy_active = true;
        privacy_active.attachment_path = Some("/private/image.jpg".to_owned());
        let mut imagery_rule = rule;
        imagery_rule.filter.attachment_required = Some(true);
        assert_eq!(
            imagery_rule.matches(&privacy_active).unwrap(),
            MatchResult::NoMatch
        );
    }
}
