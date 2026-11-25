//! Compile validated boolean predicates into parameterized two-valued SQL filters.

use crate::storage::search::{
    expression::Expression,
    predicate::{ConfidenceComparison, LabelField, Predicate, WorkflowState},
};
use anyhow::ensure;
use std::collections::{BTreeMap, BTreeSet};

const FIRST_FILTER_PARAMETER: usize = 5;
const MAX_FILTER_PARAMETERS: usize = 320;

/// Parameterized SQL predicate fragment and bound values for one expression.
pub(super) struct CompiledFilter {
    pub sql: String,
    pub values: Vec<turso::Value>,
}

/// Compiles `expression` into a two-valued SQL predicate inside the authorized camera set.
pub(super) fn compile(
    expression: &Expression<Predicate>,
    authorized_cameras: &BTreeSet<&str>,
    actor: Option<&str>,
    groups: &BTreeMap<String, BTreeSet<String>>,
) -> anyhow::Result<CompiledFilter> {
    let mut compiler = Compiler {
        authorized_cameras,
        actor,
        groups,
        values: Vec::new(),
    };
    let sql = compiler.node(expression)?;
    Ok(CompiledFilter {
        sql,
        values: compiler.values,
    })
}

struct Compiler<'a> {
    authorized_cameras: &'a BTreeSet<&'a str>,
    actor: Option<&'a str>,
    groups: &'a BTreeMap<String, BTreeSet<String>>,
    values: Vec<turso::Value>,
}

impl Compiler<'_> {
    fn node(&mut self, expression: &Expression<Predicate>) -> anyhow::Result<String> {
        Ok(match expression {
            Expression::Predicate(predicate) => self.predicate(predicate)?,
            Expression::Not(child) => format!("(NOT ({}))", self.node(child)?),
            Expression::And(children) | Expression::Or(children) => {
                let separator = if matches!(expression, Expression::And(_)) {
                    " AND "
                } else {
                    " OR "
                };
                let parts = children
                    .iter()
                    .map(|child| self.node(child))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                format!("({})", parts.join(separator))
            }
        })
    }

    fn predicate(&mut self, predicate: &Predicate) -> anyhow::Result<String> {
        Ok(match predicate {
            Predicate::Label(LabelField::Camera, values) => {
                let allowed: Vec<&str> = values
                    .iter()
                    .map(String::as_str)
                    .filter(|camera| self.authorized_cameras.contains(camera))
                    .collect();
                self.in_list("e.camera_id", allowed, false)?
            }
            Predicate::Label(LabelField::Group, values) => self.group_predicate(values)?,
            Predicate::Label(LabelField::Kind, values) => {
                self.in_list("lower(e.kind)", values.iter().map(String::as_str), false)?
            }
            Predicate::Label(LabelField::Zone, values) => {
                self.in_list("lower(e.zone)", values.iter().map(String::as_str), true)?
            }
            Predicate::Label(LabelField::EventId, values) => {
                self.in_list("e.id", values.iter().map(String::as_str), false)?
            }
            Predicate::Label(LabelField::Producer, values) => {
                self.embedding_exists_predicate("emb.producer_id", values)?
            }
            Predicate::Label(LabelField::Model, values) => {
                self.embedding_exists_predicate("emb.model_id", values)?
            }
            Predicate::Origin(values) => self.in_list("e.source", values.iter().copied(), false)?,
            Predicate::Confidence(comparison, value) => {
                let operator = match comparison {
                    ConfidenceComparison::AtLeast => ">=",
                    ConfidenceComparison::AtMost => "<=",
                };
                let parameter = self.bind(turso::Value::Real(value.get()))?;
                format!("COALESCE(e.confidence {operator} ?{parameter}, 0)")
            }
            Predicate::MissingConfidence => "(e.confidence IS NULL)".to_owned(),
            Predicate::MissingZone => "(e.zone IS NULL)".to_owned(),
            Predicate::HasImage => "(e.canonical_attachment_id IS NOT NULL)".to_owned(),
            Predicate::StartsWithin { start_ms, end_ms } => {
                let start = self.bind(turso::Value::Integer(*start_ms))?;
                let end = self.bind(turso::Value::Integer(*end_ms))?;
                format!("(e.start_time_ms >= ?{start} AND e.start_time_ms < ?{end})")
            }
            Predicate::Text { field, prefix } => self.text_predicate(*field, prefix)?,
            Predicate::Workflow(state) => self.workflow_predicate(*state)?,
        })
    }

    fn group_predicate(&mut self, values: &[String]) -> anyhow::Result<String> {
        let mut cameras = BTreeSet::new();
        for group in values {
            if let Some(members) = self.groups.get(group) {
                for camera in members {
                    if self.authorized_cameras.contains(camera.as_str()) {
                        cameras.insert(camera.as_str());
                    }
                }
            }
        }
        ensure!(
            cameras.len() <= super::MAX_CAMERAS,
            "advanced search group union exceeds the camera limit"
        );
        self.in_list("e.camera_id", cameras, false)
    }

    fn embedding_exists_predicate(
        &mut self,
        column: &str,
        values: &[String],
    ) -> anyhow::Result<String> {
        let predicate = self.in_list(column, values.iter().map(String::as_str), false)?;
        Ok(format!(
            "EXISTS (SELECT 1 FROM recording_event_embeddings AS emb \
             INDEXED BY recording_event_embeddings_event_provenance \
             WHERE emb.event_id = e.id AND COALESCE(emb.event_revision, 1) = e.revision \
             AND {predicate})"
        ))
    }

    fn text_predicate(
        &mut self,
        field: Option<&'static str>,
        prefix: &str,
    ) -> anyhow::Result<String> {
        let lower = self.bind(turso::Value::Text(prefix.to_owned()))?;
        let upper_text = format!("{prefix}\u{10ffff}");
        let upper = self.bind(turso::Value::Text(upper_text))?;
        let field_clause = match field {
            Some(field) => {
                let field_param = self.bind(turso::Value::Text(field.to_owned()))?;
                format!(" AND t.field = ?{field_param}")
            }
            None => String::new(),
        };
        Ok(format!(
            "EXISTS (SELECT 1 FROM recording_event_search_terms AS t \
             WHERE t.event_id = e.id{field_clause} \
             AND t.normalized_value >= ?{lower} AND t.normalized_value < ?{upper})"
        ))
    }

    fn workflow_predicate(&mut self, state: WorkflowState) -> anyhow::Result<String> {
        match state {
            WorkflowState::Reviewed => {
                let actor = self.require_actor()?;
                let actor_param = self.bind(turso::Value::Text(actor.to_owned()))?;
                Ok(format!(
                    "COALESCE((SELECT review.reviewed FROM event_reviews AS review \
                     WHERE review.source_id = e.camera_id AND review.event_id = e.id \
                     AND review.principal_id = ?{actor_param}), 0) = 1"
                ))
            }
            WorkflowState::Dismissed => {
                let actor = self.require_actor()?;
                let actor_param = self.bind(turso::Value::Text(actor.to_owned()))?;
                Ok(format!(
                    "COALESCE((SELECT review.dismissed FROM event_reviews AS review \
                     WHERE review.source_id = e.camera_id AND review.event_id = e.id \
                     AND review.principal_id = ?{actor_param}), 0) = 1"
                ))
            }
            WorkflowState::Bookmarked => Ok("EXISTS (SELECT 1 FROM event_bookmarks AS bookmark \
                 WHERE bookmark.source_id = e.camera_id AND bookmark.event_id = e.id \
                 AND bookmark.active = 1)"
                .to_owned()),
            WorkflowState::BookmarkedByMe => {
                let actor = self.require_actor()?;
                let actor_param = self.bind(turso::Value::Text(actor.to_owned()))?;
                Ok(format!(
                    "EXISTS (SELECT 1 FROM event_bookmarks AS bookmark \
                     WHERE bookmark.source_id = e.camera_id AND bookmark.event_id = e.id \
                     AND bookmark.active = 1 AND bookmark.created_by = ?{actor_param})"
                ))
            }
        }
    }

    fn require_actor(&self) -> anyhow::Result<&str> {
        let actor = self.actor.ok_or_else(|| {
            anyhow::anyhow!("workflow predicates require an active workspace identity")
        })?;
        super::super::workflow::validate_id(actor)?;
        Ok(actor)
    }

    fn in_list<'a>(
        &mut self,
        column: &str,
        values: impl IntoIterator<Item = &'a str>,
        nullable: bool,
    ) -> anyhow::Result<String> {
        let mut placeholders = Vec::new();
        for value in values {
            let parameter = self.bind(turso::Value::Text(value.to_owned()))?;
            placeholders.push(format!("?{parameter}"));
        }
        if placeholders.is_empty() {
            return Ok("(0 = 1)".to_owned());
        }
        let expr = format!("{column} IN ({})", placeholders.join(", "));
        if nullable {
            Ok(format!("COALESCE({expr}, 0)"))
        } else {
            Ok(format!("({expr})"))
        }
    }

    fn bind(&mut self, value: turso::Value) -> anyhow::Result<usize> {
        ensure!(
            self.values.len() < MAX_FILTER_PARAMETERS,
            "advanced search filter parameter budget exceeded"
        );
        self.values.push(value);
        Ok(FIRST_FILTER_PARAMETER + self.values.len() - 1)
    }
}
