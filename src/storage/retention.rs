//! Bounded retention decisions for whole, independently decodable recording intervals.
//!
//! Durations run from the recording interval's exclusive end. Canonical events are matched
//! explicitly by type and camera/stream identity. Detection metadata never implies motion.
//! These decisions do not delete media, override evidence holds, or activate application settings.

use crate::storage::metadata::TimelineEvent;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Maximum independent rules evaluated for one camera.
pub const MAX_RULES: usize = 16;
/// Maximum canonical event revisions accepted in one recording decision.
pub const MAX_EVENTS: usize = 256;
const MAX_SELECTOR_BYTES: usize = 128;

/// A nonempty half-open UTC interval, measured in integer milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    start_ms: i64,
    end_ms: i64,
}

impl Interval {
    pub const fn start_ms(self) -> i64 {
        self.start_ms
    }

    pub const fn end_ms(self) -> i64 {
        self.end_ms
    }

    pub fn new(start_ms: i64, end_ms: i64) -> Result<Self> {
        if start_ms >= end_ms {
            bail!("retention interval must have a start before its end");
        }
        Ok(Self { start_ms, end_ms })
    }

    const fn overlaps(self, other: Self) -> bool {
        self.start_ms < other.end_ms && other.start_ms < self.end_ms
    }
}

/// Exact canonical event evidence required by a rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Predicate {
    Continuous,
    Motion,
    /// Event types include canonical labels such as `person` and `package`.
    Event {
        event_type: Box<str>,
    },
}

/// A validated rule; zero duration disables it without falling back to inheritance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawRule")]
pub struct Rule {
    id: Box<str>,
    duration_ms: i64,
    predicate: Predicate,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    id: Box<str>,
    duration_ms: u64,
    predicate: Predicate,
}

impl TryFrom<RawRule> for Rule {
    type Error = anyhow::Error;

    fn try_from(raw: RawRule) -> Result<Self> {
        Self::new(raw.id, raw.duration_ms, raw.predicate)
    }
}

impl Rule {
    pub fn new(id: impl Into<Box<str>>, duration_ms: u64, predicate: Predicate) -> Result<Self> {
        let id = id.into();
        validate_selector(&id)?;
        if let Predicate::Event { event_type } = &predicate {
            validate_selector(event_type)?;
        }
        let duration_ms = i64::try_from(duration_ms)?;
        Ok(Self {
            id,
            duration_ms,
            predicate,
        })
    }

    fn matches(&self, event: &TimelineEvent) -> bool {
        match &self.predicate {
            Predicate::Continuous => false,
            Predicate::Motion => event.kind == "motion",
            Predicate::Event { event_type } => event.kind == event_type.as_ref(),
        }
    }
}

pub(super) fn validate_selector(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_SELECTOR_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("retention selectors must contain 1 to 128 ASCII identifier bytes");
    }
    Ok(())
}

/// Immutable rules for one effective camera policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPolicy")]
pub struct Policy {
    rules: Box<[Rule]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    #[serde(default, deserialize_with = "bounded_rules")]
    rules: Vec<Rule>,
}

fn bounded_rules<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Rule>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Vec<Rule>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("at most 16 retention rules")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            let mut rules = Vec::with_capacity(MAX_RULES);
            while let Some(rule) = sequence.next_element()? {
                if rules.len() == MAX_RULES {
                    return Err(serde::de::Error::custom("retention rule limit exceeded"));
                }
                rules.push(rule);
            }
            Ok(rules)
        }
    }
    deserializer.deserialize_seq(Visitor)
}

impl TryFrom<RawPolicy> for Policy {
    type Error = anyhow::Error;

    fn try_from(raw: RawPolicy) -> Result<Self> {
        Self::new(raw.rules)
    }
}

/// One finalized recording interval and its already committed retention obligation.
#[derive(Debug, Clone, Copy)]
pub struct Recording<'a> {
    pub camera_id: &'a str,
    pub stream_id: &'a str,
    pub interval: Interval,
    pub protected: bool,
    pub committed_deadline_ms: Option<i64>,
}

/// A policy decision, independent of storage admission, filesystem ownership, and deletion.
#[derive(Debug, PartialEq, Eq)]
pub struct Decision<'a> {
    pub deadline_ms: Option<i64>,
    pub matching_rules: Vec<&'a str>,
    pub protected: bool,
    pub reason: Reason,
}

/// The obligation that controls the resulting retention decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    MatchingRules,
    CommittedDeadline,
    Protected,
    NoMatchingEvidence,
}

impl Decision<'_> {
    /// Reports policy expiration only; storage must separately authorize deletion.
    pub fn expired(&self, now_ms: i64) -> bool {
        !self.protected && self.deadline_ms.is_none_or(|deadline| now_ms >= deadline)
    }
}

impl Policy {
    pub fn new(rules: Vec<Rule>) -> Result<Self> {
        if rules.len() > MAX_RULES {
            bail!("retention rule limit exceeded");
        }
        // ponytail: The duplicate-ID scan is bounded to 16 rules; index IDs if that ceiling grows.
        for (index, rule) in rules.iter().enumerate() {
            if rules[..index].iter().any(|previous| previous.id == rule.id) {
                bail!("retention rule IDs must be unique");
            }
        }
        Ok(Self {
            rules: rules.into_boxed_slice(),
        })
    }

    /// Evaluates an immutable snapshot of canonical current event revisions.
    ///
    /// Event types match exactly. Camera events with no stream apply camera-wide; events with
    /// a stream apply only to that logical recording stream. Open events extend to the recording
    /// end; a finite zero-duration pulse covers one millisecond. Invalid or duplicated in-scope
    /// revisions fail rather than fabricating evidence. Existing deadlines can only extend.
    pub fn resolve<'a>(
        &'a self,
        recording: Recording<'_>,
        events: &[TimelineEvent],
    ) -> Result<Decision<'a>> {
        let observations = recording_events(recording, events)?;
        let mut deadline_ms = None;
        let mut matching_rules = Vec::with_capacity(self.rules.len());
        for rule in &self.rules {
            if rule.duration_ms == 0 {
                continue;
            }
            let matches = rule.predicate == Predicate::Continuous
                || observations.iter().any(|(interval, event)| {
                    recording.interval.overlaps(*interval) && rule.matches(event)
                });
            if matches {
                let deadline = recording
                    .interval
                    .end_ms
                    .checked_add(rule.duration_ms)
                    .ok_or_else(|| anyhow::anyhow!("retention deadline exceeds the UTC range"))?;
                deadline_ms =
                    Some(deadline_ms.map_or(deadline, |previous: i64| previous.max(deadline)));
                matching_rules.push(rule.id.as_ref());
            }
        }
        let reason = if recording.protected {
            Reason::Protected
        } else if recording
            .committed_deadline_ms
            .is_some_and(|previous| deadline_ms.is_none_or(|candidate| previous >= candidate))
        {
            Reason::CommittedDeadline
        } else if deadline_ms.is_some() {
            Reason::MatchingRules
        } else {
            Reason::NoMatchingEvidence
        };
        let deadline_ms = deadline_ms
            .into_iter()
            .chain(recording.committed_deadline_ms)
            .max();
        Ok(Decision {
            deadline_ms,
            matching_rules,
            protected: recording.protected,
            reason,
        })
    }
}

fn recording_events<'a>(
    recording: Recording<'_>,
    events: &'a [TimelineEvent],
) -> Result<Vec<(Interval, &'a TimelineEvent)>> {
    if events.len() > MAX_EVENTS {
        bail!("retention event snapshot limit exceeded");
    }
    if recording.camera_id.is_empty() || recording.stream_id.is_empty() {
        bail!("retention requires a stable camera and stream identity");
    }
    let mut observations: Vec<(Interval, &TimelineEvent)> = Vec::with_capacity(events.len());
    for event in events {
        if event.camera_id != recording.camera_id
            || event
                .stream
                .as_deref()
                .is_some_and(|stream| stream != recording.stream_id)
        {
            continue;
        }
        // ponytail: Duplicate checks cover at most 256 entries. Index IDs if that limit grows.
        if event.id.is_empty()
            || observations
                .iter()
                .any(|(_, previous)| previous.id == event.id)
        {
            bail!("retention needs unique canonical current event revisions");
        }
        let end_ms = match event.end_time_ms {
            Some(end) if end < event.start_time_ms => {
                bail!("retention event ends before it starts")
            }
            Some(end) if end == event.start_time_ms => end
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("retention pulse exceeds the UTC range"))?,
            Some(end) => end,
            None if event.start_time_ms >= recording.interval.end_ms => continue,
            None => recording.interval.end_ms,
        };
        observations.push((Interval::new(event.start_time_ms, end_ms)?, event));
    }
    Ok(observations)
}
