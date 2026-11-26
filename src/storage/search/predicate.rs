//! Validate typed event predicates, detect contradictions, hash, and summarize expressions.

use super::{
    EventSearchField, MAX_FILTER_VALUES, MAX_MODEL_ID_BYTES, MAX_SEARCH_WINDOW_MS, MAX_TERM_BYTES,
    expression::{Expression, InvalidExpression, normalize},
    normalize_filter_values, normalize_search_text, normalize_source_ids,
};
use crate::storage::metadata::EventSource;
use sha2::{Digest, Sha256};

/// Maximum total UTF-8 bytes allowed across all text values in one expression.
pub const MAX_EXPRESSION_TEXT_BYTES: usize = 16 * 1_024;
const MAX_SUMMARY_BYTES: usize = 2_048;

/// Indexed label fields supported by boolean predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LabelField {
    Camera,
    Group,
    Kind,
    Zone,
    Producer,
    Model,
}

/// Comparison direction for a confidence threshold predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfidenceComparison {
    AtLeast,
    AtMost,
}

/// Workflow state predicates evaluated for the current principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkflowState {
    Reviewed,
    Dismissed,
    Bookmarked,
    BookmarkedByMe,
}

/// Unvalidated predicate input parsed from a wire request or draft.
#[derive(Debug, Clone, PartialEq)]
pub enum PredicateInput {
    Label(LabelField, Vec<String>),
    Origin(Vec<EventSource>),
    Confidence(ConfidenceComparison, f64),
    MissingConfidence,
    MissingZone,
    HasImage,
    StartsWithin {
        start_ms: i64,
        end_ms: i64,
    },
    Text {
        field: Option<EventSearchField>,
        prefix: String,
    },
    Workflow(WorkflowState),
}

/// Validated, normalized predicate ready for canonical hashing and catalog compilation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Predicate {
    Label(LabelField, Vec<String>),
    Origin(Vec<&'static str>),
    Confidence(ConfidenceComparison, Confidence),
    MissingConfidence,
    MissingZone,
    HasImage,
    StartsWithin {
        start_ms: i64,
        end_ms: i64,
    },
    Text {
        field: Option<&'static str>,
        prefix: String,
    },
    Workflow(WorkflowState),
}

/// Exact finite confidence value in `[0.0, 1.0]` with signed zero unified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Confidence(u64);

impl Confidence {
    /// Returns the stored finite confidence threshold.
    pub const fn get(self) -> f64 {
        f64::from_bits(self.0)
    }
}

/// Validates and normalizes a versioned expression AST without mutating `draft`.
///
/// # Errors
///
/// Returns `InvalidExpression` when bounds, predicate values, or direct range checks fail.
pub fn validate_expression(
    version: u32,
    draft: &Expression<PredicateInput>,
) -> Result<Expression<Predicate>, InvalidExpression> {
    let mut state = Validator {
        values_left: MAX_FILTER_VALUES,
        raw_text_left: MAX_EXPRESSION_TEXT_BYTES,
        canonical_text_left: MAX_EXPRESSION_TEXT_BYTES,
    };
    let normalized = normalize(version, draft, |predicate| state.predicate(predicate))?;
    check_contradictions(&normalized)?;
    Ok(normalized)
}

/// Computes a stable SHA-256 hex digest for a validated normalized expression.
pub fn canonical_hash(version: u32, expression: &Expression<Predicate>) -> String {
    let mut hasher = Sha256::new();
    update_bytes(&mut hasher, b"keeppeek-event-expression");
    update_bytes(&mut hasher, &version.to_le_bytes());
    hash_node(&mut hasher, expression);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Produces a deterministic bounded plain-language summary of a normalized expression.
pub fn summarize_expression(expression: &Expression<Predicate>) -> String {
    let mut summary = summarize_node(expression, false);
    if summary.len() > MAX_SUMMARY_BYTES {
        let mut end = MAX_SUMMARY_BYTES.saturating_sub(3);
        while end > 0 && !summary.is_char_boundary(end) {
            end -= 1;
        }
        summary.truncate(end);
        summary.push_str("...");
    }
    summary
}

struct Validator {
    values_left: usize,
    raw_text_left: usize,
    canonical_text_left: usize,
}

impl Validator {
    fn predicate(&mut self, input: &PredicateInput) -> Result<Predicate, InvalidExpression> {
        match input {
            PredicateInput::Label(field, values) => self.label(*field, values),
            PredicateInput::Origin(values) => {
                self.consume_list(values.len())?;
                let mut normalized: Vec<_> = values.iter().map(|value| value.as_str()).collect();
                normalized.sort_unstable();
                normalized.dedup();
                Ok(Predicate::Origin(normalized))
            }
            PredicateInput::Confidence(comparison, value) => {
                if !value.is_finite() || !(0.0..=1.0).contains(value) {
                    return Err(InvalidExpression::Predicate);
                }
                let canonical = if *value == 0.0 { 0.0 } else { *value };
                Ok(Predicate::Confidence(
                    *comparison,
                    Confidence(canonical.to_bits()),
                ))
            }
            PredicateInput::MissingConfidence => Ok(Predicate::MissingConfidence),
            PredicateInput::MissingZone => Ok(Predicate::MissingZone),
            PredicateInput::HasImage => Ok(Predicate::HasImage),
            PredicateInput::StartsWithin { start_ms, end_ms } => {
                if !end_ms
                    .checked_sub(*start_ms)
                    .is_some_and(|span| (1..=MAX_SEARCH_WINDOW_MS).contains(&span))
                {
                    return Err(InvalidExpression::Predicate);
                }
                Ok(Predicate::StartsWithin {
                    start_ms: *start_ms,
                    end_ms: *end_ms,
                })
            }
            PredicateInput::Text { field, prefix } => {
                consume_text(&mut self.raw_text_left, [prefix.as_str()], MAX_TERM_BYTES)?;
                let normalized =
                    normalize_search_text(prefix).map_err(|_| InvalidExpression::Predicate)?;
                consume_text(
                    &mut self.canonical_text_left,
                    [normalized.as_str()],
                    MAX_TERM_BYTES,
                )?;
                Ok(Predicate::Text {
                    field: field.map(EventSearchField::as_str),
                    prefix: normalized,
                })
            }
            PredicateInput::Workflow(state) => Ok(Predicate::Workflow(*state)),
        }
    }

    fn label(&mut self, field: LabelField, raw: &[String]) -> Result<Predicate, InvalidExpression> {
        self.consume_list(raw.len())?;
        let item_limit = match field {
            LabelField::Model | LabelField::Producer => MAX_MODEL_ID_BYTES,
            _ => MAX_TERM_BYTES,
        };
        consume_text(
            &mut self.raw_text_left,
            raw.iter().map(String::as_str),
            item_limit,
        )?;
        let mut values = raw.to_vec();
        match field {
            LabelField::Camera | LabelField::Group => {
                normalize_source_ids(&mut values).map_err(|_| InvalidExpression::Predicate)?;
            }
            LabelField::Kind | LabelField::Zone => {
                normalize_filter_values(&mut values, "label")
                    .map_err(|_| InvalidExpression::Predicate)?;
            }
            LabelField::Producer | LabelField::Model => {
                normalize_bounded_ids(&mut values, item_limit)?;
            }
        }
        consume_text(
            &mut self.canonical_text_left,
            values.iter().map(String::as_str),
            item_limit,
        )?;
        Ok(Predicate::Label(field, values))
    }

    fn consume_list(&mut self, count: usize) -> Result<(), InvalidExpression> {
        if count == 0 {
            return Err(InvalidExpression::Predicate);
        }
        self.values_left = self
            .values_left
            .checked_sub(count)
            .ok_or(InvalidExpression::ValueCount)?;
        Ok(())
    }
}

fn normalize_bounded_ids(
    values: &mut Vec<String>,
    max_bytes: usize,
) -> Result<(), InvalidExpression> {
    for value in &mut *values {
        *value = value.trim().to_owned();
        if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
            return Err(InvalidExpression::Predicate);
        }
    }
    values.sort_unstable();
    values.dedup();
    Ok(())
}

fn consume_text<'a>(
    remaining: &mut usize,
    values: impl IntoIterator<Item = &'a str>,
    item_limit: usize,
) -> Result<(), InvalidExpression> {
    for value in values {
        if value.len() > item_limit {
            return Err(InvalidExpression::TextBytes);
        }
        *remaining = remaining
            .checked_sub(value.len())
            .ok_or(InvalidExpression::TextBytes)?;
    }
    Ok(())
}

fn check_contradictions(expression: &Expression<Predicate>) -> Result<(), InvalidExpression> {
    match expression {
        Expression::Predicate(_) => Ok(()),
        Expression::Not(child) => check_contradictions(child),
        Expression::Or(children) => {
            for child in children {
                check_contradictions(child)?;
            }
            Ok(())
        }
        Expression::And(children) => {
            let mut min_confidence: Option<f64> = None;
            let mut max_confidence: Option<f64> = None;
            let mut min_start: Option<i64> = None;
            let mut max_end: Option<i64> = None;
            for child in children {
                check_contradictions(child)?;
                if let Expression::Predicate(predicate) = child {
                    match predicate {
                        Predicate::Confidence(ConfidenceComparison::AtLeast, value) => {
                            let threshold = value.get();
                            min_confidence =
                                Some(min_confidence.map_or(threshold, |cur| cur.max(threshold)));
                        }
                        Predicate::Confidence(ConfidenceComparison::AtMost, value) => {
                            let threshold = value.get();
                            max_confidence =
                                Some(max_confidence.map_or(threshold, |cur| cur.min(threshold)));
                        }
                        Predicate::StartsWithin { start_ms, end_ms } => {
                            min_start = Some(min_start.map_or(*start_ms, |cur| cur.max(*start_ms)));
                            max_end = Some(max_end.map_or(*end_ms, |cur| cur.min(*end_ms)));
                        }
                        _ => {}
                    }
                }
            }
            if let (Some(min), Some(max)) = (min_confidence, max_confidence)
                && min > max
            {
                return Err(InvalidExpression::ContradictoryRange);
            }
            if let (Some(start), Some(end)) = (min_start, max_end)
                && start >= end
            {
                return Err(InvalidExpression::ContradictoryRange);
            }
            Ok(())
        }
    }
}

fn update_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    let len = u64::try_from(bytes.len()).expect("hash segment fits in u64");
    hasher.update(len.to_le_bytes());
    hasher.update(bytes);
}

fn hash_node(hasher: &mut Sha256, node: &Expression<Predicate>) {
    match node {
        Expression::Predicate(predicate) => {
            update_bytes(hasher, b"P");
            hash_predicate(hasher, predicate);
        }
        Expression::Not(child) => {
            update_bytes(hasher, b"N");
            hash_node(hasher, child);
        }
        Expression::And(children) => {
            update_bytes(hasher, b"A");
            update_bytes(hasher, &(children.len() as u32).to_le_bytes());
            for child in children {
                hash_node(hasher, child);
            }
        }
        Expression::Or(children) => {
            update_bytes(hasher, b"O");
            update_bytes(hasher, &(children.len() as u32).to_le_bytes());
            for child in children {
                hash_node(hasher, child);
            }
        }
    }
}

fn hash_predicate(hasher: &mut Sha256, predicate: &Predicate) {
    match predicate {
        Predicate::Label(field, values) => {
            update_bytes(hasher, label_name(*field).as_bytes());
            update_bytes(hasher, &(values.len() as u32).to_le_bytes());
            for value in values {
                update_bytes(hasher, value.as_bytes());
            }
        }
        Predicate::Origin(values) => {
            update_bytes(hasher, b"origin");
            update_bytes(hasher, &(values.len() as u32).to_le_bytes());
            for value in values {
                update_bytes(hasher, value.as_bytes());
            }
        }
        Predicate::Confidence(comparison, value) => {
            let tag = match comparison {
                ConfidenceComparison::AtLeast => b"conf_ge".as_slice(),
                ConfidenceComparison::AtMost => b"conf_le".as_slice(),
            };
            update_bytes(hasher, tag);
            update_bytes(hasher, &value.0.to_le_bytes());
        }
        Predicate::MissingConfidence => update_bytes(hasher, b"missing_conf"),
        Predicate::MissingZone => update_bytes(hasher, b"missing_zone"),
        Predicate::HasImage => update_bytes(hasher, b"has_image"),
        Predicate::StartsWithin { start_ms, end_ms } => {
            update_bytes(hasher, b"starts_within");
            update_bytes(hasher, &start_ms.to_le_bytes());
            update_bytes(hasher, &end_ms.to_le_bytes());
        }
        Predicate::Text { field, prefix } => {
            update_bytes(hasher, b"text");
            update_bytes(hasher, field.unwrap_or("*").as_bytes());
            update_bytes(hasher, prefix.as_bytes());
        }
        Predicate::Workflow(state) => {
            update_bytes(hasher, b"workflow");
            update_bytes(hasher, workflow_name(*state).as_bytes());
        }
    }
}

fn summarize_node(node: &Expression<Predicate>, wrap: bool) -> String {
    match node {
        Expression::Predicate(predicate) => summarize_predicate(predicate),
        Expression::Not(child) => format!("NOT ({})", summarize_node(child, false)),
        Expression::And(children) => {
            let joined = children
                .iter()
                .map(|child| summarize_node(child, true))
                .collect::<Vec<_>>()
                .join(" AND ");
            if wrap { format!("({joined})") } else { joined }
        }
        Expression::Or(children) => {
            let joined = children
                .iter()
                .map(|child| summarize_node(child, true))
                .collect::<Vec<_>>()
                .join(" OR ");
            if wrap { format!("({joined})") } else { joined }
        }
    }
}

fn summarize_predicate(predicate: &Predicate) -> String {
    match predicate {
        Predicate::Label(field, values) => {
            format!("{} in [{}]", label_name(*field), values.join(", "))
        }
        Predicate::Origin(values) => format!("origin in [{}]", values.join(", ")),
        Predicate::Confidence(ConfidenceComparison::AtLeast, value) => {
            format!("confidence >= {:.2}", value.get())
        }
        Predicate::Confidence(ConfidenceComparison::AtMost, value) => {
            format!("confidence <= {:.2}", value.get())
        }
        Predicate::MissingConfidence => "confidence is missing".to_owned(),
        Predicate::MissingZone => "zone is missing".to_owned(),
        Predicate::HasImage => "has image".to_owned(),
        Predicate::StartsWithin { start_ms, end_ms } => {
            format!("time in [{start_ms}, {end_ms})")
        }
        Predicate::Text { field, prefix } => field.map_or_else(
            || format!("text matches \"{prefix}\""),
            |field| format!("{field} matches \"{prefix}\""),
        ),
        Predicate::Workflow(state) => format!("workflow is {}", workflow_name(*state)),
    }
}

const fn label_name(field: LabelField) -> &'static str {
    match field {
        LabelField::Camera => "camera",
        LabelField::Group => "group",
        LabelField::Kind => "type",
        LabelField::Zone => "zone",
        LabelField::Producer => "producer",
        LabelField::Model => "model",
    }
}

const fn workflow_name(state: WorkflowState) -> &'static str {
    match state {
        WorkflowState::Reviewed => "reviewed",
        WorkflowState::Dismissed => "dismissed",
        WorkflowState::Bookmarked => "bookmarked",
        WorkflowState::BookmarkedByMe => "bookmarked_by_me",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::search::expression::EXPRESSION_VERSION;

    fn leaf(input: PredicateInput) -> Expression<PredicateInput> {
        Expression::Predicate(input)
    }

    #[test]
    fn normalizes_predicates_and_produces_stable_hash_and_summary() {
        let first = Expression::And(vec![
            leaf(PredicateInput::Label(
                LabelField::Kind,
                vec![" Person ".into(), "person".into()],
            )),
            leaf(PredicateInput::Confidence(
                ConfidenceComparison::AtLeast,
                0.75,
            )),
            leaf(PredicateInput::Text {
                field: Some(EventSearchField::Text),
                prefix: "  Red   Van  ".into(),
            }),
        ]);
        let second = Expression::And(vec![
            leaf(PredicateInput::Text {
                field: Some(EventSearchField::Text),
                prefix: "red van".into(),
            }),
            leaf(PredicateInput::Confidence(
                ConfidenceComparison::AtLeast,
                0.75,
            )),
            leaf(PredicateInput::Label(
                LabelField::Kind,
                vec!["PERSON".into()],
            )),
        ]);

        let normalized_first = validate_expression(EXPRESSION_VERSION, &first).unwrap();
        let normalized_second = validate_expression(EXPRESSION_VERSION, &second).unwrap();
        assert_eq!(normalized_first, normalized_second);
        assert_eq!(
            canonical_hash(EXPRESSION_VERSION, &normalized_first),
            canonical_hash(EXPRESSION_VERSION, &normalized_second)
        );
        let summary = summarize_expression(&normalized_first);
        assert!(summary.contains("type in [person]"), "{summary}");
        assert!(summary.contains("confidence >= 0.75"), "{summary}");
        assert!(summary.contains("text matches \"red van\""), "{summary}");
    }

    #[test]
    fn rejects_contradictory_confidence_and_time_ranges_and_non_finite_values() {
        let bad_conf = Expression::And(vec![
            leaf(PredicateInput::Confidence(
                ConfidenceComparison::AtLeast,
                0.8,
            )),
            leaf(PredicateInput::Confidence(
                ConfidenceComparison::AtMost,
                0.5,
            )),
        ]);
        assert_eq!(
            validate_expression(EXPRESSION_VERSION, &bad_conf),
            Err(InvalidExpression::ContradictoryRange)
        );

        let bad_time = Expression::And(vec![
            leaf(PredicateInput::StartsWithin {
                start_ms: 1_000,
                end_ms: 2_000,
            }),
            leaf(PredicateInput::StartsWithin {
                start_ms: 3_000,
                end_ms: 4_000,
            }),
        ]);
        assert_eq!(
            validate_expression(EXPRESSION_VERSION, &bad_time),
            Err(InvalidExpression::ContradictoryRange)
        );

        let nan_conf = leaf(PredicateInput::Confidence(
            ConfidenceComparison::AtLeast,
            f64::NAN,
        ));
        assert_eq!(
            validate_expression(EXPRESSION_VERSION, &nan_conf),
            Err(InvalidExpression::Predicate)
        );
    }

    #[test]
    fn enforces_value_count_and_text_byte_limits_before_dedup() {
        let over_values = Expression::Or(vec![
            leaf(PredicateInput::Label(
                LabelField::Zone,
                vec!["porch".into(); 32],
            )),
            leaf(PredicateInput::Label(
                LabelField::Zone,
                vec!["porch".into(); 33],
            )),
        ]);
        assert_eq!(
            validate_expression(EXPRESSION_VERSION, &over_values),
            Err(InvalidExpression::ValueCount)
        );

        let over_bytes = leaf(PredicateInput::Text {
            field: None,
            prefix: "a".repeat(MAX_TERM_BYTES + 1),
        });
        assert_eq!(
            validate_expression(EXPRESSION_VERSION, &over_bytes),
            Err(InvalidExpression::TextBytes)
        );
    }
}
