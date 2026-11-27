//! Serialization-layer secret redaction for KeepPeek MCP responses.
//!
//! Every JSON-RPC response and error passes through [`SecretRedactor`] before
//! serialization. Raw secret values, URL-encoded secret variants, URL userinfo
//! credentials, and values stored under sensitive keys are replaced with
//! `[REDACTED]`, while `{secret:KEY}` and `{secret:KEY|url}` references remain
//! untouched.

use crate::{
    config::{Secrets, is_secret_reference},
    logging::LogHub,
};
use serde_json::Value;

/// Marker used in place of redacted sensitive values.
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";
const DEPTH_LIMIT_PLACEHOLDER: &str = "[REDACTED:DEPTH_LIMIT]";
const MAX_REDACTION_DEPTH: usize = 64;
const MIN_RAW_SUBSTRING_SECRET_BYTES: usize = 4;

const OPAQUE_ID_KEYS: &[&str] = &[
    "plan_id",
    "configuration_revision",
    "expected_configuration_revision",
    "discovery_id",
    "template_id",
    "rule_id",
    "event_id",
];

const SENSITIVE_KEY_FRAGMENTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "access_key",
    "verifier",
    "cookie",
    "authorization",
    "api_key",
    "apikey",
    "private_key",
    "credential",
    "passphrase",
    "signing_key",
    "grant",
];

const INLINE_KEY_MARKERS: &[&str] = &[
    "password=",
    "passwd=",
    "secret=",
    "token=",
    "access_key=",
    "verifier=",
    "cookie=",
    "authorization=",
    "api_key=",
    "apikey=",
    "private_key=",
    "grant=",
];

/// Redacts raw secrets across strings and nested JSON values.
#[derive(Clone, Default)]
pub struct SecretRedactor {
    sensitive_values: Vec<String>,
    log_hub: Option<LogHub>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RedactionPath {
    Root,
    ResultObject,
    ResultContentArray,
    ResultContentItem,
    Other,
}

impl SecretRedactor {
    /// Builds a redactor from `secrets`, optional extra raw values, and an optional `LogHub`.
    #[must_use]
    pub fn new(
        secrets: &Secrets,
        extra_values: impl IntoIterator<Item = String>,
        log_hub: Option<LogHub>,
    ) -> Self {
        let mut sensitive_values = Vec::new();
        for value in secrets.redaction_values().into_iter().chain(extra_values) {
            push_sensitive_variants(&mut sensitive_values, value);
        }
        sensitive_values.sort_unstable_by(|left, right| {
            right.len().cmp(&left.len()).then_with(|| left.cmp(right))
        });
        sensitive_values.dedup();
        Self {
            sensitive_values,
            log_hub,
        }
    }

    /// Recursively redacts `value` in place up to the bounded nesting depth.
    pub fn redact_value(&self, value: &mut Value) {
        self.redact_value_at_depth(value, 0, false, RedactionPath::Root);
    }

    /// Redacts raw secrets, URL credentials, and inline secret pairs in `text`.
    #[must_use]
    pub fn redact_str(&self, text: &str) -> String {
        if text.is_empty() || is_secret_reference(text) {
            return text.to_owned();
        }
        let mut redacted = text.to_owned();
        for sensitive in &self.sensitive_values {
            if !sensitive.is_empty() {
                redacted = redacted.replace(sensitive, REDACTED_PLACEHOLDER);
            }
        }
        redacted = redact_url_userinfo(&redacted);
        redacted = redact_inline_assignments(&redacted);
        if let Some(hub) = &self.log_hub {
            redacted = apply_log_hub_preserving_secret_refs(hub, &redacted);
        }
        redacted
    }

    fn redact_opaque_id_value(&self, value: &mut Value) {
        if let Value::String(text) = value {
            if text.is_empty() || is_secret_reference(text) {
                return;
            }
            // Skip raw-secret substring matching so opaque IDs stay intact, but still scrub URL/inline secrets.
            let mut redacted = redact_url_userinfo(text);
            redacted = redact_inline_assignments(&redacted);
            *text = redacted;
        } else {
            self.redact_value_at_depth(value, 1, false, RedactionPath::Other);
        }
    }

    fn redact_value_at_depth(
        &self,
        value: &mut Value,
        depth: usize,
        under_sensitive_key: bool,
        path: RedactionPath,
    ) {
        if depth > MAX_REDACTION_DEPTH {
            *value = Value::String(DEPTH_LIMIT_PLACEHOLDER.to_owned());
            return;
        }
        match value {
            Value::Null | Value::Bool(_) => {}
            Value::Number(_) => {
                if under_sensitive_key {
                    *value = Value::String(REDACTED_PLACEHOLDER.to_owned());
                }
            }
            Value::String(text) => {
                if under_sensitive_key {
                    if !text.is_empty() && !is_secret_reference(text) {
                        *text = REDACTED_PLACEHOLDER.to_owned();
                    }
                } else {
                    *text = self.redact_str(text);
                }
            }
            Value::Array(items) => {
                let item_path = if path == RedactionPath::ResultContentArray {
                    RedactionPath::ResultContentItem
                } else {
                    RedactionPath::Other
                };
                for item in items {
                    self.redact_value_at_depth(item, depth + 1, under_sensitive_key, item_path);
                }
            }
            Value::Object(entries) => {
                let is_top_level_image_content = path == RedactionPath::ResultContentItem
                    && entries.get("type").and_then(Value::as_str) == Some("image");
                for (key, entry_value) in entries {
                    if is_top_level_image_content && key == "data" {
                        continue;
                    }
                    if is_opaque_id_key(key) {
                        self.redact_opaque_id_value(entry_value);
                        continue;
                    }
                    let next_path = match (path, key.as_str()) {
                        (RedactionPath::Root, "result") => RedactionPath::ResultObject,
                        (RedactionPath::ResultObject, "content") => {
                            RedactionPath::ResultContentArray
                        }
                        _ => RedactionPath::Other,
                    };
                    let key_sensitive = is_sensitive_key(key);
                    self.redact_value_at_depth(entry_value, depth + 1, key_sensitive, next_path);
                }
            }
        }
    }
}

fn push_sensitive_variants(target: &mut Vec<String>, value: String) {
    if value.len() < MIN_RAW_SUBSTRING_SECRET_BYTES {
        return;
    }
    let form_encoded: String = url::form_urlencoded::byte_serialize(value.as_bytes()).collect();
    let rfc3986_encoded = percent_encode_rfc3986(&value);
    target.push(value);
    if form_encoded.len() >= MIN_RAW_SUBSTRING_SECRET_BYTES {
        target.push(form_encoded);
    }
    if rfc3986_encoded.len() >= MIN_RAW_SUBSTRING_SECRET_BYTES {
        target.push(rfc3986_encoded);
    }
}

fn is_opaque_id_key(key: &str) -> bool {
    OPAQUE_ID_KEYS
        .iter()
        .any(|opaque| key.eq_ignore_ascii_case(opaque))
}

fn percent_encode_rfc3986(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "next_page_token" | "page_token" | "secret_keys_count"
    ) || lower.ends_with("_configured")
        || lower.ends_with("_count")
    {
        return false;
    }
    SENSITIVE_KEY_FRAGMENTS
        .iter()
        .any(|fragment| lower.contains(fragment))
}

fn redact_url_userinfo(text: &str) -> String {
    let mut redacted = text.to_owned();
    let mut search_from = 0;
    while let Some(scheme_offset) = redacted[search_from..].find("://") {
        let authority_start = search_from + scheme_offset + 3;
        let authority_end = redacted[authority_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '/' | '?' | '#')
            })
            .map_or(redacted.len(), |offset| authority_start + offset);
        let Some(at_offset) = redacted[authority_start..authority_end].rfind('@') else {
            search_from = authority_end;
            continue;
        };
        let userinfo_end = authority_start + at_offset;
        let userinfo = &redacted[authority_start..userinfo_end];
        if userinfo_uses_secret_reference(userinfo) {
            search_from = userinfo_end + 1;
            continue;
        }
        redacted.replace_range(authority_start..userinfo_end, REDACTED_PLACEHOLDER);
        search_from = authority_start + REDACTED_PLACEHOLDER.len() + 1;
    }
    redacted
}

fn userinfo_uses_secret_reference(userinfo: &str) -> bool {
    if is_secret_reference(userinfo) {
        return true;
    }
    let Some((user, pass)) = userinfo.split_once(':') else {
        return false;
    };
    !user.is_empty() && is_secret_reference(pass)
}

fn redact_inline_assignments(text: &str) -> String {
    let mut redacted = text.to_owned();
    for marker in INLINE_KEY_MARKERS {
        let mut search_from = 0;
        loop {
            let lowercase = redacted[search_from..].to_ascii_lowercase();
            let Some(offset) = lowercase.find(marker) else {
                break;
            };
            let start = search_from + offset;
            let value_start = start + marker.len();
            let value_end = redacted[value_start..]
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, '&' | ',' | ';' | '"' | '\'')
                })
                .map_or(redacted.len(), |offset| value_start + offset);
            let candidate = &redacted[value_start..value_end];
            if is_secret_reference(candidate) {
                search_from = value_end;
            } else {
                redacted.replace_range(value_start..value_end, REDACTED_PLACEHOLDER);
                search_from = value_start + REDACTED_PLACEHOLDER.len();
            }
            if search_from >= redacted.len() {
                break;
            }
        }
    }
    redacted
}

fn apply_log_hub_preserving_secret_refs(hub: &LogHub, text: &str) -> String {
    if !text.contains("{secret:") {
        return hub.redact_output(text);
    }
    let mut preserved = Vec::new();
    let masked = mask_secret_ref_spans(text, &mut preserved);
    let mut output = hub.redact_output(&masked);
    for (token, original) in preserved {
        output = output.replace(&token, &original);
    }
    output
}

fn mask_secret_ref_spans(text: &str, preserved: &mut Vec<(String, String)>) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let Some(ref_offset) = rest.find("{secret:") else {
            output.push_str(rest);
            break;
        };
        let ref_start = cursor + ref_offset;
        let Some(close_offset) = text[ref_start..].find('}') else {
            output.push_str(rest);
            break;
        };
        let ref_end = ref_start + close_offset + 1;
        let candidate = &text[ref_start..ref_end];
        if !is_secret_reference(candidate) {
            output.push_str(&text[cursor..ref_end]);
            cursor = ref_end;
            continue;
        }
        let span_start = expand_prefix_for_ref(text, cursor, ref_start);
        let span_end = if text[ref_end..].starts_with('@') {
            ref_end + 1
        } else {
            ref_end
        };
        output.push_str(&text[cursor..span_start]);
        let placeholder = format!("__KEEPPEEK_SECRET_REF_{}__", preserved.len());
        preserved.push((placeholder.clone(), text[span_start..span_end].to_owned()));
        output.push_str(&placeholder);
        cursor = span_end;
    }
    output
}

fn expand_prefix_for_ref(text: &str, cursor: usize, ref_start: usize) -> usize {
    let prefix = &text[cursor..ref_start];
    if let Some(scheme) = prefix.rfind("://") {
        let authority_start = cursor + scheme + 3;
        if !text[authority_start..ref_start]
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '/' | '?' | '#' | '@'))
        {
            return authority_start;
        }
    }
    let lower_prefix = prefix.to_ascii_lowercase();
    for marker in INLINE_KEY_MARKERS {
        if lower_prefix.ends_with(marker) {
            return ref_start - marker.len();
        }
    }
    ref_start
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn redacts_raw_and_url_encoded_secrets_while_preserving_references() {
        let secrets = Secrets::from_map(BTreeMap::from([
            ("CAMERA_PASSWORD".to_owned(), "s3cr3t/p@ss word!".to_owned()),
            (
                "KEEPPEEK_ACCESS_KEY".to_owned(),
                "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            ),
        ]));
        let redactor = SecretRedactor::new(&secrets, Vec::new(), None);
        let mut payload = json!({
            "password": "raw-password-value",
            "password_ref": "{secret:CAMERA_PASSWORD}",
            "password_configured": true,
            "next_page_token": "cursor-42",
            "rtsp_url_raw": "rtsp://admin:s3cr3t%2Fp%40ss%20word%21@192.0.2.10/main",
            "rtsp_url_ref": "rtsp://admin:{secret:CAMERA_PASSWORD|url}@192.0.2.10/main",
            "error_text": "failed with s3cr3t/p@ss word! and key 550e8400-e29b-41d4-a716-446655440000",
            "nested": {
                "api_key": "plain-api-key",
                "verifier": "sha256-hash",
                "cookie": "session=abc",
                "authorization": "Bearer xyz"
            }
        });

        redactor.redact_value(&mut payload);

        let serialized = serde_json::to_string(&payload).unwrap();
        assert!(!serialized.contains("s3cr3t/p@ss word!"));
        assert!(!serialized.contains("s3cr3t%2Fp%40ss%20word%21"));
        assert!(!serialized.contains("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!serialized.contains("raw-password-value"));
        assert!(!serialized.contains("plain-api-key"));
        assert!(!serialized.contains("sha256-hash"));
        assert!(!serialized.contains("session=abc"));
        assert!(!serialized.contains("Bearer xyz"));

        assert_eq!(payload["password"], REDACTED_PLACEHOLDER);
        assert_eq!(payload["password_ref"], "{secret:CAMERA_PASSWORD}");
        assert_eq!(payload["password_configured"], true);
        assert_eq!(payload["next_page_token"], "cursor-42");
        assert_eq!(
            payload["rtsp_url_ref"],
            "rtsp://admin:{secret:CAMERA_PASSWORD|url}@192.0.2.10/main"
        );
        assert_eq!(payload["rtsp_url_raw"], "rtsp://[REDACTED]@192.0.2.10/main");
    }

    #[test]
    fn redacts_with_log_hub_and_preserves_secret_references() {
        let hub = LogHub::default();
        hub.set_sensitive_values(["top-secret-hub-value".to_owned()]);
        let redactor = SecretRedactor::new(&Secrets::default(), Vec::new(), Some(hub));

        let output = redactor.redact_str(
            "url=rtsp://admin:{secret:CAM_PASS|url}@192.0.2.10 detail=top-secret-hub-value",
        );
        assert!(output.contains("{secret:CAM_PASS|url}"));
        assert!(!output.contains("top-secret-hub-value"));
        assert!(output.contains(REDACTED_PLACEHOLDER));
    }

    #[test]
    fn enforces_maximum_recursion_depth() {
        let redactor = SecretRedactor::new(&Secrets::default(), Vec::new(), None);
        let mut value = json!("leaf");
        for _ in 0..70 {
            value = json!({ "child": value });
        }
        redactor.redact_value(&mut value);
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(serialized.contains(DEPTH_LIMIT_PLACEHOLDER));
    }

    #[test]
    fn preserves_top_level_image_base64_and_redacts_nested_and_opaque_urls() {
        let secrets = Secrets::from_map(BTreeMap::from([
            ("TINY".to_owned(), "ab".to_owned()),
            ("LONG_PASS".to_owned(), "SuperSecret123!".to_owned()),
        ]));
        let redactor = SecretRedactor::new(&secrets, Vec::new(), None);
        let image_data = "abSuperSecret123!Base64Payload==";
        let mut payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [
                    {
                        "type": "image",
                        "mimeType": "image/jpeg",
                        "data": image_data
                    }
                ],
                "structuredContent": {
                    "plan_id": "plan-ab-SuperSecret123!",
                    "configuration_revision": "rev-ab-999",
                    "template_id": "rtsp://admin:hunter2@192.0.2.10/main",
                    "rule_id": "password=raw-inline-secret",
                    "nested_log_fields": {
                        "type": "image",
                        "data": "leaked SuperSecret123! in log field"
                    },
                    "password": "ab",
                    "detail": "cable leaked SuperSecret123! near label ab"
                }
            }
        });

        redactor.redact_value(&mut payload);

        assert_eq!(payload["result"]["content"][0]["data"], image_data);
        let structured = &payload["result"]["structuredContent"];
        assert_eq!(structured["plan_id"], "plan-ab-SuperSecret123!");
        assert_eq!(structured["configuration_revision"], "rev-ab-999");
        assert_eq!(
            structured["template_id"],
            "rtsp://[REDACTED]@192.0.2.10/main"
        );
        assert_eq!(structured["rule_id"], "password=[REDACTED]");
        assert_eq!(
            structured["nested_log_fields"]["data"],
            "leaked [REDACTED] in log field"
        );
        assert_eq!(structured["password"], REDACTED_PLACEHOLDER);
        assert_eq!(
            structured["detail"],
            "cable leaked [REDACTED] near label ab"
        );
    }
}
