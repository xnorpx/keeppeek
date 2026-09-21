//! Resolves retention deadlines from bounded, source-scoped UTC evidence.
//!
//! A deadline is not permission to delete a recording. The catalog must establish
//! evidence completeness and atomically recheck holds and revisions before removal.

use serde::{Deserialize, Serialize};

const RULES_MAX: usize = 16;
const EVIDENCE_MAX: usize = 256;

/// A nonempty half-open UTC interval in integer milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    start_ms: i64,
    end_ms: i64,
}

impl Interval {
    pub fn new(start_ms: i64, end_ms: i64) -> anyhow::Result<Self> {
        anyhow::ensure!(start_ms < end_ms, "retention interval must be nonempty");
        Ok(Self { start_ms, end_ms })
    }

    pub const fn start_ms(self) -> i64 {
        self.start_ms
    }

    pub const fn end_ms(self) -> i64 {
        self.end_ms
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let start_ms = self.start_ms.max(other.start_ms);
        let end_ms = self.end_ms.min(other.end_ms);
        (start_ms < end_ms).then_some(Self { start_ms, end_ms })
    }
}

/// The evidence class that selects a rule's time window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleClass {
    Continuous,
    Motion,
    Alert,
    Detection,
}

/// Additional evidence required inside the selected time window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionMode {
    All,
    Motion,
    ActiveObjects,
}

/// A normalized event fact, not a detector-specific label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    Motion,
    Alert,
    Detection,
    ActiveObject,
}

/// Evidence must belong to the same camera and stream as the evaluated media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evidence {
    kind: EvidenceKind,
    interval: Interval,
}

impl Evidence {
    pub const fn new(kind: EvidenceKind, interval: Interval) -> Self {
        Self { kind, interval }
    }
}

/// A validated rule with an exact duration; zero disables this rule only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionRule {
    class: RuleClass,
    duration_ms: i64,
    mode: RetentionMode,
}

impl RetentionRule {
    pub fn new(class: RuleClass, duration_ms: u64, mode: RetentionMode) -> anyhow::Result<Self> {
        let duration_ms = i64::try_from(duration_ms)?;
        Ok(Self {
            class,
            duration_ms,
            mode,
        })
    }

    fn matches_class(&self, kind: EvidenceKind) -> bool {
        match self.class {
            RuleClass::Continuous => true,
            RuleClass::Motion => matches!(kind, EvidenceKind::Motion | EvidenceKind::ActiveObject),
            RuleClass::Alert => kind == EvidenceKind::Alert,
            RuleClass::Detection => kind == EvidenceKind::Detection,
        }
    }

    fn eligible_end(&self, window: Interval, evidence: &[Evidence]) -> Option<i64> {
        if self.mode == RetentionMode::All {
            return Some(window.end_ms);
        }
        evidence
            .iter()
            .filter_map(|fact| {
                let eligible = match self.mode {
                    RetentionMode::All => true,
                    RetentionMode::Motion => {
                        matches!(fact.kind, EvidenceKind::Motion | EvidenceKind::ActiveObject)
                    }
                    RetentionMode::ActiveObjects => fact.kind == EvidenceKind::ActiveObject,
                };
                eligible
                    .then(|| window.intersection(fact.interval))
                    .flatten()
                    .map(|interval| interval.end_ms)
            })
            .max()
    }

    fn matching_end(&self, media: Interval, evidence: &[Evidence]) -> Option<i64> {
        if self.duration_ms == 0 {
            return None;
        }
        if self.class == RuleClass::Continuous {
            return self.eligible_end(media, evidence);
        }
        // ponytail: At most 256 evidence intervals bound this quadratic intersection scan.
        evidence
            .iter()
            .filter(|fact| self.matches_class(fact.kind))
            .filter_map(|fact| {
                media
                    .intersection(fact.interval)
                    .and_then(|window| self.eligible_end(window, evidence))
            })
            .max()
    }
}

/// A policy result that never shortens a previously committed deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionDecision {
    /// No deadline means no positive match; it does not authorize deletion.
    pub deadline_ms: Option<i64>,
}

impl RetentionDecision {
    /// Tests the deadline only; holds and evidence completeness are catalog concerns.
    pub fn expired_at(self, now_ms: i64) -> bool {
        self.deadline_ms.is_some_and(|deadline| now_ms >= deadline)
    }
}

/// An enabled retention policy, distinct from disabled legacy rollout behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    rules: Box<[RetentionRule]>,
}

impl RetentionPolicy {
    pub fn new(rules: Vec<RetentionRule>) -> anyhow::Result<Self> {
        anyhow::ensure!(rules.len() <= RULES_MAX, "too many retention rules");
        Ok(Self {
            rules: rules.into_boxed_slice(),
        })
    }

    /// Resolves one whole object's deadline without allocating or accessing storage.
    ///
    /// The caller must supply complete evidence for the same source and stream.
    /// Disabled policy rollout must bypass this resolver, not pass an empty rule list.
    /// Errors leave the caller's prior committed decision unchanged.
    pub fn resolve(
        &self,
        media: Interval,
        evidence: &[Evidence],
        committed_deadline_ms: Option<i64>,
    ) -> anyhow::Result<RetentionDecision> {
        anyhow::ensure!(
            evidence.len() <= EVIDENCE_MAX,
            "too much retention evidence"
        );
        let mut deadline_ms = committed_deadline_ms;
        for rule in &self.rules {
            let Some(end_ms) = rule.matching_end(media, evidence) else {
                continue;
            };
            let candidate = end_ms
                .checked_add(rule.duration_ms)
                .ok_or_else(|| anyhow::anyhow!("retention deadline exceeds UTC range"))?;
            deadline_ms = Some(deadline_ms.map_or(candidate, |prior| prior.max(candidate)));
        }
        Ok(RetentionDecision { deadline_ms })
    }
}
