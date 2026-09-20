//! Approved state-store document schemas (*#39* slice 3).
//!
//! Generic namespaces accept only approved schemas, and every approved schema
//! allowlists its fields and types. There is no generic secret detector:
//! documents cannot smuggle credentials, media bytes, SDP, or logs because no
//! approved field can represent them. Field strings are length-bounded and
//! reject control characters. Service-specific `parameters` are rejected until
//! registered per-service validators exist: an incomplete credential-key
//! blacklist cannot cover explicit shapes like `vendor.api_key`, so only an
//! absent or empty parameters object is accepted.

use super::state_store::{Error, Invalid};
use prost_types::{Struct, Value, value::Kind};

pub(super) const MEDIA_INTENT_SCHEMA: &str = "keeppeek.media-intent.v1";

const MAX_ID_CHARS: usize = 128;
const MAX_PRIORITY: f64 = 1_000_000.0;

pub(super) fn validate_value(schema: &str, value: &Struct) -> Result<(), Error> {
    match schema {
        MEDIA_INTENT_SCHEMA => validate_media_intent(value),
        _ => Err(Error::Invalid(Invalid::Schema)),
    }
}

fn validate_media_intent(value: &Struct) -> Result<(), Error> {
    let role = require_string(value, "role")?;
    if role != "publish" && role != "subscribe" {
        return Err(Error::Invalid(Invalid::Schema));
    }
    require_bool(value, "desired")?;
    let source_id = require_string(value, "source_id")?;
    require_identifier(&source_id)?;
    match require_string(value, "media_kind")?.as_str() {
        "audio" | "video" => {}
        _ => return Err(Error::Invalid(Invalid::Schema)),
    }
    match value.fields.get("recording_mode") {
        None if role == "publish" => return Err(Error::Invalid(Invalid::Schema)),
        None => {}
        Some(_) if role != "publish" => return Err(Error::Invalid(Invalid::Schema)),
        Some(mode) => match string_value(mode) {
            Some(mode) if mode == "inherit" || mode == "disabled" || mode == "required" => {}
            _ => return Err(Error::Invalid(Invalid::Schema)),
        },
    }
    for name in ["stream_id", "variant_id", "output_profile"] {
        if let Some(field) = value.fields.get(name) {
            let text = string_value(field).ok_or(Error::Invalid(Invalid::Schema))?;
            require_text(&text)?;
        }
    }
    if let Some(field) = value.fields.get("priority") {
        match field.kind.as_ref() {
            Some(Kind::NumberValue(priority))
                if priority.is_finite() && *priority >= 0.0 && *priority <= MAX_PRIORITY => {}
            _ => return Err(Error::Invalid(Invalid::Schema)),
        }
    }
    if let Some(field) = value.fields.get("parameters") {
        match field.kind.as_ref() {
            Some(Kind::StructValue(parameters)) if parameters.fields.is_empty() => {}
            _ => return Err(Error::Invalid(Invalid::Schema)),
        }
    }
    for name in value.fields.keys() {
        if !matches!(
            name.as_str(),
            "role"
                | "desired"
                | "source_id"
                | "media_kind"
                | "recording_mode"
                | "stream_id"
                | "variant_id"
                | "output_profile"
                | "priority"
                | "parameters"
        ) {
            return Err(Error::Invalid(Invalid::Schema));
        }
    }
    Ok(())
}

fn require_string(value: &Struct, name: &str) -> Result<String, Error> {
    value
        .fields
        .get(name)
        .and_then(string_value)
        .ok_or(Error::Invalid(Invalid::Schema))
}

fn require_bool(value: &Struct, name: &str) -> Result<bool, Error> {
    value
        .fields
        .get(name)
        .and_then(|field| match field.kind.as_ref() {
            Some(Kind::BoolValue(enabled)) => Some(*enabled),
            _ => None,
        })
        .ok_or(Error::Invalid(Invalid::Schema))
}

fn string_value(field: &Value) -> Option<String> {
    match field.kind.as_ref() {
        Some(Kind::StringValue(text)) => Some(text.clone()),
        _ => None,
    }
}

fn require_identifier(text: &str) -> Result<(), Error> {
    if text.is_empty() {
        return Err(Error::Invalid(Invalid::Schema));
    }
    require_text(text)
}

fn require_text(text: &str) -> Result<(), Error> {
    if text.chars().count() > MAX_ID_CHARS || has_control(text) {
        return Err(Error::Invalid(Invalid::Schema));
    }
    Ok(())
}

fn has_control(text: &str) -> bool {
    text.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn string_field(name: &str, text: &str) -> (String, Value) {
        (
            name.to_owned(),
            Value {
                kind: Some(Kind::StringValue(text.to_owned())),
            },
        )
    }

    fn bool_field(name: &str, value: bool) -> (String, Value) {
        (
            name.to_owned(),
            Value {
                kind: Some(Kind::BoolValue(value)),
            },
        )
    }

    fn number_field(name: &str, value: f64) -> (String, Value) {
        (
            name.to_owned(),
            Value {
                kind: Some(Kind::NumberValue(value)),
            },
        )
    }

    fn valid_publish() -> Struct {
        Struct {
            fields: BTreeMap::from([
                string_field("role", "publish"),
                bool_field("desired", true),
                string_field("source_id", "front-door"),
                string_field("media_kind", "video"),
                string_field("recording_mode", "disabled"),
            ]),
        }
    }

    fn valid_subscribe() -> Struct {
        Struct {
            fields: BTreeMap::from([
                string_field("role", "subscribe"),
                bool_field("desired", false),
                string_field("source_id", "back-door"),
                string_field("media_kind", "audio"),
            ]),
        }
    }

    #[test]
    fn valid_intents_are_accepted() {
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &valid_publish()),
            Ok(())
        );
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &valid_subscribe()),
            Ok(())
        );
    }

    #[test]
    fn unknown_schemas_are_denied() {
        assert_eq!(
            validate_value("keeppeek.unknown.v1", &valid_publish()),
            Err(Error::Invalid(Invalid::Schema))
        );
        assert_eq!(
            validate_value("", &valid_publish()),
            Err(Error::Invalid(Invalid::Schema))
        );
    }

    #[test]
    fn malformed_intents_are_rejected() {
        let mut missing_role = valid_publish();
        missing_role.fields.remove("role");
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &missing_role),
            Err(Error::Invalid(Invalid::Schema))
        );
        let mut bad_role = valid_publish();
        bad_role.fields.insert(
            "role".to_owned(),
            Value {
                kind: Some(Kind::StringValue("admin".to_owned())),
            },
        );
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &bad_role),
            Err(Error::Invalid(Invalid::Schema))
        );
        let mut publish_without_mode = valid_publish();
        publish_without_mode.fields.remove("recording_mode");
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &publish_without_mode),
            Err(Error::Invalid(Invalid::Schema))
        );
        let mut subscribe_with_mode = valid_subscribe();
        subscribe_with_mode.fields.insert(
            "recording_mode".to_owned(),
            Value {
                kind: Some(Kind::StringValue("disabled".to_owned())),
            },
        );
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &subscribe_with_mode),
            Err(Error::Invalid(Invalid::Schema))
        );
        let mut bad_kind = valid_publish();
        bad_kind.fields.insert(
            "media_kind".to_owned(),
            Value {
                kind: Some(Kind::StringValue("smell".to_owned())),
            },
        );
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &bad_kind),
            Err(Error::Invalid(Invalid::Schema))
        );
        let mut extra_field = valid_publish();
        extra_field.fields.insert(
            "password".to_owned(),
            Value {
                kind: Some(Kind::StringValue("hunter2".to_owned())),
            },
        );
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &extra_field),
            Err(Error::Invalid(Invalid::Schema))
        );
    }

    #[test]
    fn bounded_fields_and_parameters_are_checked() {
        let mut long_id = valid_publish();
        long_id.fields.insert(
            "source_id".to_owned(),
            Value {
                kind: Some(Kind::StringValue("s".repeat(MAX_ID_CHARS + 1))),
            },
        );
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &long_id),
            Err(Error::Invalid(Invalid::Schema))
        );
        let mut control_id = valid_publish();
        control_id.fields.insert(
            "source_id".to_owned(),
            Value {
                kind: Some(Kind::StringValue("front\ndoor".to_owned())),
            },
        );
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &control_id),
            Err(Error::Invalid(Invalid::Schema))
        );
        let mut bad_priority = valid_publish();
        let (name, field) = number_field("priority", f64::NAN);
        bad_priority.fields.insert(name, field);
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &bad_priority),
            Err(Error::Invalid(Invalid::Schema))
        );
        for key in [
            "vendor.api_key",
            "vendor.authorization",
            "vendor.url",
            "transcoder.profile",
        ] {
            let mut parameters = valid_publish();
            parameters.fields.insert(
                "parameters".to_owned(),
                Value {
                    kind: Some(Kind::StructValue(Struct {
                        fields: BTreeMap::from([string_field(key, "x")]),
                    })),
                },
            );
            assert_eq!(
                validate_value(MEDIA_INTENT_SCHEMA, &parameters),
                Err(Error::Invalid(Invalid::Schema)),
                "non-empty parameters must be rejected until service validators exist: {key}"
            );
        }
        let mut empty_parameters = valid_publish();
        empty_parameters.fields.insert(
            "parameters".to_owned(),
            Value {
                kind: Some(Kind::StructValue(Struct {
                    fields: BTreeMap::new(),
                })),
            },
        );
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &empty_parameters),
            Ok(())
        );
        let mut unstructured_parameters = valid_publish();
        unstructured_parameters.fields.insert(
            "parameters".to_owned(),
            Value {
                kind: Some(Kind::StringValue("profile=high".to_owned())),
            },
        );
        assert_eq!(
            validate_value(MEDIA_INTENT_SCHEMA, &unstructured_parameters),
            Err(Error::Invalid(Invalid::Schema))
        );
    }
}
