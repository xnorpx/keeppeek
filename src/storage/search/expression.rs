//! Validate and normalize bounded boolean event-search expressions.

use std::fmt;

/// Supported expression AST schema version.
pub const EXPRESSION_VERSION: u32 = 1;
/// Maximum allowed tree depth for an expression AST.
pub const MAX_EXPRESSION_DEPTH: u8 = 8;
/// Maximum allowed node count for an expression AST.
pub const MAX_EXPRESSION_NODES: u8 = 64;

/// A boolean expression tree evaluated inside an authorized camera and UTC scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Expression<P> {
    Predicate(P),
    And(Vec<Self>),
    Or(Vec<Self>),
    Not(Box<Self>),
}

/// Structured validation failure for a boolean event-search expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidExpression {
    UnsupportedVersion,
    EmptyGroup,
    Depth,
    NodeCount,
    Predicate,
    ValueCount,
    TextBytes,
    ContradictoryRange,
}

impl InvalidExpression {
    /// Returns a clear user-facing validation message.
    pub const fn message(self) -> &'static str {
        match self {
            Self::UnsupportedVersion => "event search expression version is not supported",
            Self::EmptyGroup => "event search boolean group must not be empty",
            Self::Depth => "event search expression depth exceeds 8",
            Self::NodeCount => "event search expression node count exceeds 64",
            Self::Predicate => "event search predicate is invalid",
            Self::ValueCount => "event search expression exceeds 64 predicate values",
            Self::TextBytes => "event search expression text exceeds 16 KiB",
            Self::ContradictoryRange => {
                "event search expression contains a contradictory confidence or time range"
            }
        }
    }
}

impl fmt::Display for InvalidExpression {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for InvalidExpression {}

/// Validates `draft` and returns a canonical expression without mutating the draft.
///
/// # Errors
///
/// Returns `InvalidExpression` when the version, depth, node count, or a predicate violates bounds.
pub fn normalize<P, Q: Ord>(
    version: u32,
    draft: &Expression<P>,
    mut normalize_predicate: impl FnMut(&P) -> Result<Q, InvalidExpression>,
) -> Result<Expression<Q>, InvalidExpression> {
    if version != EXPRESSION_VERSION {
        return Err(InvalidExpression::UnsupportedVersion);
    }
    let mut remaining = MAX_EXPRESSION_NODES;
    normalize_node(draft, 1, &mut remaining, &mut normalize_predicate)
}

fn normalize_node<P, Q: Ord>(
    node: &Expression<P>,
    depth: u8,
    remaining: &mut u8,
    normalize_predicate: &mut impl FnMut(&P) -> Result<Q, InvalidExpression>,
) -> Result<Expression<Q>, InvalidExpression> {
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(InvalidExpression::Depth);
    }
    *remaining = remaining
        .checked_sub(1)
        .ok_or(InvalidExpression::NodeCount)?;
    match node {
        Expression::Predicate(predicate) => {
            normalize_predicate(predicate).map(Expression::Predicate)
        }
        Expression::Not(child) => {
            let normalized = normalize_node(child, depth + 1, remaining, normalize_predicate)?;
            Ok(match normalized {
                Expression::Not(inner) => *inner,
                other => Expression::Not(Box::new(other)),
            })
        }
        Expression::And(children) => {
            normalize_group(children, true, depth, remaining, normalize_predicate)
        }
        Expression::Or(children) => {
            normalize_group(children, false, depth, remaining, normalize_predicate)
        }
    }
}

fn normalize_group<P, Q: Ord>(
    children: &[Expression<P>],
    is_and: bool,
    depth: u8,
    remaining: &mut u8,
    normalize_predicate: &mut impl FnMut(&P) -> Result<Q, InvalidExpression>,
) -> Result<Expression<Q>, InvalidExpression> {
    if children.is_empty() {
        return Err(InvalidExpression::EmptyGroup);
    }
    let mut normalized = Vec::with_capacity(children.len());
    for child in children {
        let item = normalize_node(child, depth + 1, remaining, normalize_predicate)?;
        match (is_and, item) {
            (true, Expression::And(nested)) => normalized.extend(nested),
            (false, Expression::Or(nested)) => normalized.extend(nested),
            (_, item) => normalized.push(item),
        }
    }
    normalized.sort();
    normalized.dedup();
    if normalized.len() == 1 {
        return Ok(normalized
            .pop()
            .expect("group has at least one child after validation"));
    }
    Ok(if is_and {
        Expression::And(normalized)
    } else {
        Expression::Or(normalized)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_commutative_groups_flattening_and_double_negation() {
        let left = Expression::And(vec![
            Expression::Predicate("b"),
            Expression::And(vec![
                Expression::Predicate("a"),
                Expression::Not(Box::new(Expression::Not(Box::new(Expression::Predicate(
                    "b",
                ))))),
            ]),
        ]);
        let right = Expression::And(vec![Expression::Predicate("a"), Expression::Predicate("b")]);

        assert_eq!(
            normalize(EXPRESSION_VERSION, &left, |value| Ok(*value)).unwrap(),
            normalize(EXPRESSION_VERSION, &right, |value| Ok(*value)).unwrap()
        );
    }

    #[test]
    fn rejects_version_empty_groups_depth_and_node_bounds() {
        assert_eq!(
            normalize(2, &Expression::Predicate("a"), |value| Ok(*value)),
            Err(InvalidExpression::UnsupportedVersion)
        );
        assert_eq!(
            normalize(EXPRESSION_VERSION, &Expression::<&str>::And(vec![]), |v| {
                Ok(*v)
            }),
            Err(InvalidExpression::EmptyGroup)
        );
        let mut deep = Expression::Predicate("leaf");
        for _ in 0..MAX_EXPRESSION_DEPTH {
            deep = Expression::Not(Box::new(deep));
        }
        assert_eq!(
            normalize(EXPRESSION_VERSION, &deep, |value| Ok(*value)),
            Err(InvalidExpression::Depth)
        );
        let wide = Expression::Or(vec![
            Expression::Predicate("a");
            usize::from(MAX_EXPRESSION_NODES)
        ]);
        assert_eq!(
            normalize(EXPRESSION_VERSION, &wide, |value| Ok(*value)),
            Err(InvalidExpression::NodeCount)
        );
    }
}
