//! Convert between WebRTC protobuf event-search messages and validated storage types.

use super::{
    ControlCommandError, millis_timestamp, required_timestamp_ms, storage_event_search_field,
    storage_event_source,
};
use crate::{
    api::proto,
    storage::{
        EventEmbedding, EventEmbeddingInput, EventEmbeddingProvenance,
        EventSearchPlannerDiagnostics, EventSearchSemanticSummary, EventSemanticRanking,
        search::{
            expression::{
                EXPRESSION_VERSION, Expression, MAX_EXPRESSION_DEPTH, MAX_EXPRESSION_NODES,
            },
            predicate::{ConfidenceComparison, LabelField, PredicateInput, WorkflowState},
        },
    },
};
use serde::{Deserialize, Serialize};

/// Converts a protobuf expression tree into an unvalidated storage AST with bounded recursion.
pub(super) fn parse_proto_expression(
    proto_expr: &proto::EventSearchExpression,
) -> Result<Expression<PredicateInput>, ControlCommandError> {
    let mut remaining = MAX_EXPRESSION_NODES;
    parse_node(proto_expr, 1, &mut remaining)
}

fn parse_node(
    expr: &proto::EventSearchExpression,
    depth: u8,
    remaining: &mut u8,
) -> Result<Expression<PredicateInput>, ControlCommandError> {
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(invalid("event search expression depth exceeds 8"));
    }
    *remaining = remaining
        .checked_sub(1)
        .ok_or_else(|| invalid("event search expression node count exceeds 64"))?;
    match &expr.node {
        Some(proto::event_search_expression::Node::AndGroup(list)) => {
            if list.children.is_empty() {
                return Err(invalid("event search boolean group must not be empty"));
            }
            let children = list
                .children
                .iter()
                .map(|child| parse_node(child, depth + 1, remaining))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::And(children))
        }
        Some(proto::event_search_expression::Node::OrGroup(list)) => {
            if list.children.is_empty() {
                return Err(invalid("event search boolean group must not be empty"));
            }
            let children = list
                .children
                .iter()
                .map(|child| parse_node(child, depth + 1, remaining))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::Or(children))
        }
        Some(proto::event_search_expression::Node::NotExpr(child)) => Ok(Expression::Not(
            Box::new(parse_node(child, depth + 1, remaining)?),
        )),
        Some(proto::event_search_expression::Node::Predicate(predicate)) => {
            parse_predicate(predicate).map(Expression::Predicate)
        }
        None => Err(invalid("event search expression node is required")),
    }
}

fn parse_predicate(
    predicate: &proto::EventSearchPredicate,
) -> Result<PredicateInput, ControlCommandError> {
    use proto::event_search_predicate::Kind;
    match &predicate.kind {
        Some(Kind::CameraIds(list)) => Ok(PredicateInput::Label(
            LabelField::Camera,
            list.values.clone(),
        )),
        Some(Kind::GroupIds(list)) => Ok(PredicateInput::Label(
            LabelField::Group,
            list.values.clone(),
        )),
        Some(Kind::EventTypes(list)) => {
            Ok(PredicateInput::Label(LabelField::Kind, list.values.clone()))
        }
        Some(Kind::Origins(list)) => {
            let origins = list
                .origins
                .iter()
                .copied()
                .map(storage_event_source)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PredicateInput::Origin(origins))
        }
        Some(Kind::Zones(list)) => Ok(PredicateInput::Label(LabelField::Zone, list.values.clone())),
        Some(Kind::MissingZone(true)) => Ok(PredicateInput::MissingZone),
        Some(Kind::MissingZone(false)) => Err(invalid(
            "use NOT around missing_zone instead of setting missing_zone to false",
        )),
        Some(Kind::Confidence(conf)) => parse_confidence_predicate(conf),
        Some(Kind::MissingConfidence(true)) => Ok(PredicateInput::MissingConfidence),
        Some(Kind::MissingConfidence(false)) => Err(invalid(
            "use NOT around missing_confidence instead of setting missing_confidence to false",
        )),
        Some(Kind::HasImage(true)) => Ok(PredicateInput::HasImage),
        Some(Kind::HasImage(false)) => Err(invalid(
            "use NOT around has_image instead of setting has_image to false",
        )),
        Some(Kind::TimeRange(range)) => {
            let start_ms =
                required_timestamp_ms(range.start_time.as_ref(), "predicate start time")?;
            let end_ms = required_timestamp_ms(range.end_time.as_ref(), "predicate end time")?;
            Ok(PredicateInput::StartsWithin { start_ms, end_ms })
        }
        Some(Kind::Text(text)) => {
            let field = text.field.map(storage_event_search_field).transpose()?;
            Ok(PredicateInput::Text {
                field,
                prefix: text.prefix.clone(),
            })
        }
        Some(Kind::Workflow(value)) => parse_workflow_predicate(*value),
        Some(Kind::ProducerIds(list)) => Ok(PredicateInput::Label(
            LabelField::Producer,
            list.values.clone(),
        )),
        Some(Kind::ModelIds(list)) => Ok(PredicateInput::Label(
            LabelField::Model,
            list.values.clone(),
        )),
        None => Err(invalid("event search predicate kind is required")),
    }
}

fn parse_confidence_predicate(
    conf: &proto::EventConfidencePredicate,
) -> Result<PredicateInput, ControlCommandError> {
    let comparison = match proto::EventConfidenceOperator::try_from(conf.operator) {
        Ok(proto::EventConfidenceOperator::AtLeast) => ConfidenceComparison::AtLeast,
        Ok(proto::EventConfidenceOperator::AtMost) => ConfidenceComparison::AtMost,
        _ => return Err(invalid("event confidence operator is invalid")),
    };
    Ok(PredicateInput::Confidence(comparison, conf.threshold))
}

fn parse_workflow_predicate(value: i32) -> Result<PredicateInput, ControlCommandError> {
    let state = match proto::EventWorkflowPredicate::try_from(value) {
        Ok(proto::EventWorkflowPredicate::Reviewed) => WorkflowState::Reviewed,
        Ok(proto::EventWorkflowPredicate::Dismissed) => WorkflowState::Dismissed,
        Ok(proto::EventWorkflowPredicate::Bookmarked) => WorkflowState::Bookmarked,
        Ok(proto::EventWorkflowPredicate::BookmarkedByMe) => WorkflowState::BookmarkedByMe,
        _ => return Err(invalid("event workflow predicate is invalid")),
    };
    Ok(PredicateInput::Workflow(state))
}

/// Converts a protobuf embedding into a validated storage embedding input with provenance.
pub(super) fn parse_embedding_input(
    embedding: Option<&proto::EventSearchEmbedding>,
) -> Result<EventEmbeddingInput, ControlCommandError> {
    let proto_emb = embedding.ok_or_else(|| invalid("event search embedding is required"))?;
    let base = super::proto_event_embedding(Some(proto_emb))?;
    let created_at_ms = proto_emb
        .created_at
        .as_ref()
        .map(|ts| required_timestamp_ms(Some(ts), "embedding created timestamp"))
        .transpose()?;
    Ok(EventEmbeddingInput {
        embedding: EventEmbedding {
            model_id: base.model_id,
            values: base.values,
        },
        provenance: EventEmbeddingProvenance {
            model_version: proto_emb.model_version.trim().to_owned(),
            producer_id: proto_emb.producer_id.trim().to_owned(),
            event_revision: proto_emb.event_revision,
            created_at_ms,
        },
    })
}

/// Converts an optional protobuf semantic ranking clause into a storage ranking input.
pub(super) fn parse_semantic_ranking(
    ranking: Option<&proto::EventSemanticRanking>,
) -> Result<Option<EventSemanticRanking>, ControlCommandError> {
    let Some(ranking) = ranking else {
        return Ok(None);
    };
    let input = parse_embedding_input(ranking.embedding.as_ref())?;
    if ranking
        .minimum_score
        .is_some_and(|score| !score.is_finite() || !(-1.0..=1.0).contains(&score))
    {
        return Err(invalid(
            "semantic minimum score must be finite and between -1 and 1",
        ));
    }
    Ok(Some(EventSemanticRanking {
        embedding: input.embedding,
        model_version: input.provenance.model_version,
        minimum_score: ranking.minimum_score,
    }))
}

/// Converts storage semantic provenance summary into protobuf.
pub(super) fn proto_semantic_summary(
    summary: EventSearchSemanticSummary,
) -> proto::EventSearchSemanticSummary {
    proto::EventSearchSemanticSummary {
        model_id: summary.model_id,
        model_version: summary.model_version,
        producer_id: summary.producer_id,
        dimensions: summary.dimensions,
        score_interpretation: summary.score_interpretation,
        minimum_score: summary.minimum_score,
        searched_start_time: Some(millis_timestamp(summary.searched_start_time_ms)),
        searched_end_time: Some(millis_timestamp(summary.searched_end_time_ms)),
        candidate_count: summary.candidate_count,
        candidates_truncated: summary.candidates_truncated,
    }
}

/// Converts Administrator-only planner diagnostics into protobuf.
pub(super) fn proto_diagnostics(
    diagnostics: EventSearchPlannerDiagnostics,
) -> proto::EventSearchPlannerDiagnostics {
    proto::EventSearchPlannerDiagnostics {
        plan_category: diagnostics.plan_category,
        estimated_candidates: diagnostics.estimated_candidates,
        scanned_candidates: diagnostics.scanned_candidates,
        wall_time_ms: diagnostics.wall_time_ms,
    }
}

/// Serialized JSON representation of a version 1 saved search expression.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum SavedExpressionDoc {
    And { children: Vec<Self> },
    Or { children: Vec<Self> },
    Not { child: Box<Self> },
    Predicate { predicate: SavedPredicateDoc },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum SavedPredicateDoc {
    CameraIds {
        values: Vec<String>,
    },
    GroupIds {
        values: Vec<String>,
    },
    EventTypes {
        values: Vec<String>,
    },
    Origins {
        values: Vec<String>,
    },
    Zones {
        values: Vec<String>,
    },
    MissingZone,
    Confidence {
        operator: String,
        threshold: f64,
    },
    MissingConfidence,
    HasImage,
    TimeRange {
        start_ms: i64,
        end_ms: i64,
    },
    Text {
        field: Option<String>,
        prefix: String,
    },
    Workflow {
        state: String,
    },
    ProducerIds {
        values: Vec<String>,
    },
    ModelIds {
        values: Vec<String>,
    },
}

/// Encodes a protobuf expression into its versioned JSON document while preserving draft order.
pub(super) fn proto_to_saved_doc(
    expr: &proto::EventSearchExpression,
) -> Result<SavedExpressionDoc, ControlCommandError> {
    let input = parse_proto_expression(expr)?;
    let _ = crate::storage::search::predicate::validate_expression(EXPRESSION_VERSION, &input)
        .map_err(|error| invalid(error.message()))?;
    Ok(input_to_saved_doc(&input))
}

fn input_to_saved_doc(expr: &Expression<PredicateInput>) -> SavedExpressionDoc {
    match expr {
        Expression::And(children) => SavedExpressionDoc::And {
            children: children.iter().map(input_to_saved_doc).collect(),
        },
        Expression::Or(children) => SavedExpressionDoc::Or {
            children: children.iter().map(input_to_saved_doc).collect(),
        },
        Expression::Not(child) => SavedExpressionDoc::Not {
            child: Box::new(input_to_saved_doc(child)),
        },
        Expression::Predicate(predicate) => SavedExpressionDoc::Predicate {
            predicate: predicate_to_saved_doc(predicate),
        },
    }
}

fn predicate_to_saved_doc(predicate: &PredicateInput) -> SavedPredicateDoc {
    match predicate {
        PredicateInput::Label(LabelField::Camera, values) => SavedPredicateDoc::CameraIds {
            values: values.clone(),
        },
        PredicateInput::Label(LabelField::Group, values) => SavedPredicateDoc::GroupIds {
            values: values.clone(),
        },
        PredicateInput::Label(LabelField::Kind, values) => SavedPredicateDoc::EventTypes {
            values: values.clone(),
        },
        PredicateInput::Label(LabelField::Zone, values) => SavedPredicateDoc::Zones {
            values: values.clone(),
        },
        PredicateInput::Label(LabelField::Producer, values) => SavedPredicateDoc::ProducerIds {
            values: values.clone(),
        },
        PredicateInput::Label(LabelField::Model, values) => SavedPredicateDoc::ModelIds {
            values: values.clone(),
        },
        PredicateInput::Origin(origins) => SavedPredicateDoc::Origins {
            values: origins
                .iter()
                .map(|origin| origin.as_str().to_owned())
                .collect(),
        },
        PredicateInput::Confidence(op, threshold) => SavedPredicateDoc::Confidence {
            operator: match op {
                ConfidenceComparison::AtLeast => "at_least".to_owned(),
                ConfidenceComparison::AtMost => "at_most".to_owned(),
            },
            threshold: *threshold,
        },
        PredicateInput::MissingConfidence => SavedPredicateDoc::MissingConfidence,
        PredicateInput::MissingZone => SavedPredicateDoc::MissingZone,
        PredicateInput::HasImage => SavedPredicateDoc::HasImage,
        PredicateInput::StartsWithin { start_ms, end_ms } => SavedPredicateDoc::TimeRange {
            start_ms: *start_ms,
            end_ms: *end_ms,
        },
        PredicateInput::Text { field, prefix } => SavedPredicateDoc::Text {
            field: field.map(|item| item.as_str().to_owned()),
            prefix: prefix.clone(),
        },
        PredicateInput::Workflow(state) => SavedPredicateDoc::Workflow {
            state: match state {
                WorkflowState::Reviewed => "reviewed".to_owned(),
                WorkflowState::Dismissed => "dismissed".to_owned(),
                WorkflowState::Bookmarked => "bookmarked".to_owned(),
                WorkflowState::BookmarkedByMe => "bookmarked_by_me".to_owned(),
            },
        },
    }
}

/// Converts a saved JSON document back into a protobuf expression tree.
pub(super) fn saved_doc_to_proto(
    doc: &SavedExpressionDoc,
) -> anyhow::Result<proto::EventSearchExpression> {
    Ok(match doc {
        SavedExpressionDoc::And { children } => proto::EventSearchExpression {
            node: Some(proto::event_search_expression::Node::AndGroup(
                proto::EventSearchExpressionList {
                    children: children
                        .iter()
                        .map(saved_doc_to_proto)
                        .collect::<anyhow::Result<Vec<_>>>()?,
                },
            )),
        },
        SavedExpressionDoc::Or { children } => proto::EventSearchExpression {
            node: Some(proto::event_search_expression::Node::OrGroup(
                proto::EventSearchExpressionList {
                    children: children
                        .iter()
                        .map(saved_doc_to_proto)
                        .collect::<anyhow::Result<Vec<_>>>()?,
                },
            )),
        },
        SavedExpressionDoc::Not { child } => proto::EventSearchExpression {
            node: Some(proto::event_search_expression::Node::NotExpr(Box::new(
                saved_doc_to_proto(child)?,
            ))),
        },
        SavedExpressionDoc::Predicate { predicate } => proto::EventSearchExpression {
            node: Some(proto::event_search_expression::Node::Predicate(
                saved_predicate_to_proto(predicate)?,
            )),
        },
    })
}

fn saved_predicate_to_proto(
    predicate: &SavedPredicateDoc,
) -> anyhow::Result<proto::EventSearchPredicate> {
    use proto::event_search_predicate::Kind;
    let kind = match predicate {
        SavedPredicateDoc::CameraIds { values } => {
            Kind::CameraIds(proto::EventStringListPredicate {
                values: values.clone(),
            })
        }
        SavedPredicateDoc::GroupIds { values } => Kind::GroupIds(proto::EventStringListPredicate {
            values: values.clone(),
        }),
        SavedPredicateDoc::EventTypes { values } => {
            Kind::EventTypes(proto::EventStringListPredicate {
                values: values.clone(),
            })
        }
        SavedPredicateDoc::Origins { values } => Kind::Origins(saved_origins_to_proto(values)?),
        SavedPredicateDoc::Zones { values } => Kind::Zones(string_list_proto(values)),
        SavedPredicateDoc::MissingZone => Kind::MissingZone(true),
        SavedPredicateDoc::Confidence {
            operator,
            threshold,
        } => Kind::Confidence(proto::EventConfidencePredicate {
            operator: match operator.as_str() {
                "at_least" => proto::EventConfidenceOperator::AtLeast as i32,
                "at_most" => proto::EventConfidenceOperator::AtMost as i32,
                _ => anyhow::bail!("saved confidence operator is invalid"),
            },
            threshold: *threshold,
        }),
        SavedPredicateDoc::MissingConfidence => Kind::MissingConfidence(true),
        SavedPredicateDoc::HasImage => Kind::HasImage(true),
        SavedPredicateDoc::TimeRange { start_ms, end_ms } => {
            Kind::TimeRange(proto::EventTimeRangePredicate {
                start_time: Some(millis_timestamp(*start_ms)),
                end_time: Some(millis_timestamp(*end_ms)),
            })
        }
        SavedPredicateDoc::Text { field, prefix } => {
            Kind::Text(saved_text_to_proto(field.as_deref(), prefix)?)
        }
        SavedPredicateDoc::Workflow { state } => Kind::Workflow(saved_workflow_to_proto(state)?),
        SavedPredicateDoc::ProducerIds { values } => Kind::ProducerIds(string_list_proto(values)),
        SavedPredicateDoc::ModelIds { values } => Kind::ModelIds(string_list_proto(values)),
    };
    Ok(proto::EventSearchPredicate { kind: Some(kind) })
}

fn string_list_proto(values: &[String]) -> proto::EventStringListPredicate {
    proto::EventStringListPredicate {
        values: values.to_vec(),
    }
}

fn saved_origins_to_proto(values: &[String]) -> anyhow::Result<proto::EventOriginListPredicate> {
    let origins = values
        .iter()
        .map(|value| match value.as_str() {
            "camera" => Ok(proto::EventOrigin::Camera as i32),
            "keeppeek" => Ok(proto::EventOrigin::Keeppeek as i32),
            _ => anyhow::bail!("saved event origin is invalid"),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(proto::EventOriginListPredicate { origins })
}

fn saved_text_to_proto(
    field: Option<&str>,
    prefix: &str,
) -> anyhow::Result<proto::EventTextPredicate> {
    let field = field
        .map(|value| match value {
            "event_type" => Ok(proto::EventSearchField::EventType as i32),
            "face_name" => Ok(proto::EventSearchField::FaceName as i32),
            "object_class" => Ok(proto::EventSearchField::ObjectClass as i32),
            "text" => Ok(proto::EventSearchField::Text as i32),
            _ => anyhow::bail!("saved text field is invalid"),
        })
        .transpose()?;
    Ok(proto::EventTextPredicate {
        prefix: prefix.to_owned(),
        field,
    })
}

fn saved_workflow_to_proto(state: &str) -> anyhow::Result<i32> {
    match state {
        "reviewed" => Ok(proto::EventWorkflowPredicate::Reviewed as i32),
        "dismissed" => Ok(proto::EventWorkflowPredicate::Dismissed as i32),
        "bookmarked" => Ok(proto::EventWorkflowPredicate::Bookmarked as i32),
        "bookmarked_by_me" => Ok(proto::EventWorkflowPredicate::BookmarkedByMe as i32),
        _ => anyhow::bail!("saved workflow predicate is invalid"),
    }
}

/// Collects explicit camera IDs and group IDs referenced anywhere in a saved document.
pub(super) fn collect_references(
    doc: &SavedExpressionDoc,
    camera_ids: &mut Vec<String>,
    group_ids: &mut Vec<String>,
) {
    match doc {
        SavedExpressionDoc::And { children } | SavedExpressionDoc::Or { children } => {
            for child in children {
                collect_references(child, camera_ids, group_ids);
            }
        }
        SavedExpressionDoc::Not { child } => collect_references(child, camera_ids, group_ids),
        SavedExpressionDoc::Predicate { predicate } => match predicate {
            SavedPredicateDoc::CameraIds { values } => camera_ids.extend(values.iter().cloned()),
            SavedPredicateDoc::GroupIds { values } => group_ids.extend(values.iter().cloned()),
            _ => {}
        },
    }
}

fn invalid(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}
