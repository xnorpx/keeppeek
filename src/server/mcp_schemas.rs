//! Complete MCP JSON Schema definitions for all 21 KeepPeek MCP tools.
//!
//! Every tool declares explicit parameter properties, types, bounds, enums, and
//! secret-reference constraints so LLM clients can construct valid requests
//! without guessing hidden field names.

use crate::mcp::protocol::{ToolAnnotations, ToolDefinition};
use serde_json::{Value, json};

const SECRET_REF_PATTERN: &str = r"^\{secret:[A-Z_][A-Z0-9_]*(\|url)?\}$";

/// Returns the tool definitions visible to the caller's role.
pub(super) fn tool_definitions(is_admin: bool) -> Vec<ToolDefinition> {
    let mut tools = user_tool_definitions();
    if !is_admin {
        return tools;
    }
    tools.extend(admin_read_tool_definitions());
    tools.extend(admin_diagnostics_tool_definitions());
    tools.extend(admin_config_write_tool_definitions());
    tools.extend(admin_management_write_tool_definitions());
    tools
}

fn tool_def(
    name: &str,
    description: &str,
    input_schema: Value,
    annotations: ToolAnnotations,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        annotations,
    }
}

fn user_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_def(
            "get_server_health",
            "Read canonical KeepPeek server and camera health (filtered to permitted cameras for User role).",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "search_events",
            "Search stored timeline events across permitted cameras with bounded result limits.",
            search_events_schema(),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "get_recording_coverage",
            "Inspect recorded intervals and coverage gaps for permitted cameras over a bounded time window.",
            recording_coverage_schema(),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "get_camera_snapshot",
            "Capture one privacy-gated, downscaled JPEG snapshot for a permitted camera when [mcp].allow_snapshots is true. Size is bounded by [mcp].max_snapshot_bytes.",
            json!({
                "type": "object",
                "required": ["camera_id"],
                "properties": {
                    "camera_id": { "type": "string", "minLength": 1, "maxLength": 256 },
                    "source_preference": {
                        "type": "string",
                        "enum": ["auto", "live", "isapi", "onvif", "reolink", "latest_event"]
                    }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::read_only(),
        ),
    ]
}

fn search_events_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "camera_id": { "type": "string" },
            "camera_ids": { "type": "array", "items": { "type": "string" } },
            "source_id": { "type": "string" },
            "source_ids": { "type": "array", "items": { "type": "string" } },
            "event_types": { "type": "array", "items": { "type": "string" } },
            "query": { "type": "string" },
            "text": { "type": "string" },
            "stream_id": { "type": "string", "enum": ["main", "sub"] },
            "start_ms": { "type": "integer" },
            "end_ms": { "type": "integer" },
            "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
            "page_token": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn recording_coverage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "camera_id": { "type": "string" },
            "start_ms": { "type": "integer", "minimum": 0 },
            "end_ms": { "type": "integer", "minimum": 0 },
            "limit": { "type": "integer", "minimum": 1, "maximum": 50 },
            "page_size": { "type": "integer", "minimum": 1, "maximum": 50 },
            "max_segments": { "type": "integer", "minimum": 1, "maximum": 256 },
            "minimum_gap_ms": { "type": "integer", "minimum": 0 },
            "stream": { "type": "string", "enum": ["main", "sub"] },
            "state": { "type": "string" },
            "group": { "type": "string" },
            "search": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn admin_read_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_def(
            "search_logs",
            "Search the bounded redacted server log buffer by level, target, camera, query, sequence, or timestamp.",
            json!({
                "type": "object",
                "properties": {
                    "level": { "type": "string", "enum": ["trace", "debug", "info", "warn", "error"] },
                    "target": { "type": "string" },
                    "camera": { "type": "string" },
                    "query": { "type": "string" },
                    "since_sequence": { "type": "integer", "minimum": 0 },
                    "since_timestamp_ms": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "get_logging_settings",
            "Read the active log filter directive, default filter, version, and log buffer statistics.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "get_configuration",
            "Read a paginated KeepPeek configuration snapshot with effective values, templates, and secret references preserved.",
            json!({
                "type": "object",
                "properties": {
                    "page_size": { "type": "integer", "minimum": 1, "maximum": 64 },
                    "page_token": { "type": "string" }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "list_secret_keys",
            "List sorted secret key names stored in secrets.toml without exposing any secret values.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "plan_configuration_change",
            "Dry-run a camera, default, or template configuration change without writing files; works even in read-only mode.",
            plan_configuration_change_schema(),
            ToolAnnotations::read_only(),
        ),
    ]
}

fn admin_diagnostics_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_def(
            "get_storage_status",
            "Inspect storage configuration, disk pressure, named volumes, archive status, and recording maintenance state.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "get_notification_status",
            "Summarize notification rules, cloud push status, and MQTT delivery health without exposing secrets.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "discover_cameras",
            "Run a bounded read-only LAN camera discovery probe across private IPv4 subnets or list discoverable networks.",
            json!({
                "type": "object",
                "properties": {
                    "list_networks_only": { "type": "boolean" },
                    "networks": { "type": "array", "items": { "type": "string" } },
                    "subnets": { "type": "array", "items": { "type": "integer", "minimum": 0, "maximum": 255 } },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 10 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                },
                "additionalProperties": false
            }),
            ToolAnnotations {
                read_only_hint: true,
                destructive_hint: false,
                idempotent_hint: true,
                open_world_hint: true,
            },
        ),
        tool_def(
            "get_runtime_configuration",
            "Read runtime storage, retention, and listener settings and revision.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "list_runtime_profiles",
            "List saved runtime camera profiles and active profile selection.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ToolAnnotations::read_only(),
        ),
    ]
}

fn admin_config_write_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_def(
            "apply_configuration_plan",
            "Apply a previously validated configuration plan by plan_id and expected_configuration_revision.",
            json!({
                "type": "object",
                "required": ["plan_id", "expected_configuration_revision"],
                "properties": {
                    "plan_id": { "type": "string" },
                    "expected_configuration_revision": { "type": "string" }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::write(true, false),
        ),
        tool_def(
            "update_runtime_configuration",
            "Update runtime host, port, or storage settings; always requires expected_configuration_revision.",
            update_runtime_configuration_schema(),
            ToolAnnotations::write(true, false),
        ),
        tool_def(
            "manage_configuration_template",
            "Save, duplicate, or delete a camera configuration template using {secret:KEY} references only.",
            manage_template_schema(),
            ToolAnnotations::write(true, false),
        ),
    ]
}

fn admin_management_write_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_def(
            "manage_camera",
            "Add, update, or remove a camera by IP address. Credentials must use {secret:KEY} or {secret:KEY|url} references only.",
            manage_camera_schema(),
            ToolAnnotations::write(true, false),
        ),
        tool_def(
            "manage_notification_rule",
            "List, save a draft, activate, or delete a notification rule. Secret destinations must use {secret:KEY} references or preserve destination_ref.",
            manage_notification_rule_schema(),
            ToolAnnotations::write(true, false),
        ),
        tool_def(
            "set_runtime_profile",
            "Switch the active runtime camera profile or revert to the base profile using expected_revision.",
            json!({
                "type": "object",
                "required": ["expected_revision"],
                "properties": {
                    "expected_revision": { "type": "integer", "minimum": 0 },
                    "profile_id": { "type": ["string", "null"] },
                    "definition_revision": { "type": "integer", "minimum": 0 },
                    "definition_digest": { "type": "string" }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::write(false, true),
        ),
        tool_def(
            "set_logging_filter",
            "Update and persist the live tracing log filter directive.",
            json!({
                "type": "object",
                "required": ["filter"],
                "properties": {
                    "filter": { "type": "string", "minLength": 1 }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::write(false, true),
        ),
    ]
}

fn tristate_schema(value_schema: Value) -> Value {
    json!({
        "oneOf": [
            value_schema,
            {
                "type": "object",
                "properties": {
                    "set": value_schema,
                    "clear": { "type": "boolean" }
                },
                "additionalProperties": false
            }
        ]
    })
}

fn secret_ref_string_schema() -> Value {
    json!({
        "type": "string",
        "pattern": SECRET_REF_PATTERN,
        "description": "Secret reference formatted as {secret:KEY} or {secret:KEY|url}; raw credentials are rejected."
    })
}

fn plan_configuration_change_schema() -> Value {
    json!({
        "type": "object",
        "required": ["expected_configuration_revision", "change"],
        "properties": {
            "expected_configuration_revision": { "type": "string", "minLength": 1 },
            "targets": {
                "type": "object",
                "description": "Specify exactly one selector field (omit when changing defaults).",
                "properties": {
                    "camera_ids": { "type": "array", "items": { "type": "string" } },
                    "group_id": { "type": "string" },
                    "all_cameras": { "type": "boolean" },
                    "filtered_cameras": {
                        "type": "object",
                        "properties": {
                            "search": { "type": "string" },
                            "backend": { "type": "string", "enum": ["auto", "retina", "reo_proto"] },
                            "recording_mode": {
                                "type": "string",
                                "enum": ["sub", "main", "both", "event_boost", "off", "event_only"]
                            }
                        },
                        "additionalProperties": false
                    }
                },
                "additionalProperties": false
            },
            "change": {
                "type": "object",
                "description": "Specify exactly one of patch, template_id, or defaults.",
                "properties": {
                    "patch": camera_patch_schema(),
                    "template_id": { "type": "string" },
                    "defaults": camera_defaults_patch_schema()
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

fn camera_patch_schema() -> Value {
    let backend_enum = json!({ "type": "string", "enum": ["auto", "retina", "reo_proto"] });
    let transport_enum = json!({ "type": "string", "enum": ["tcp", "udp"] });
    let mode_enum = json!({
        "type": "string",
        "enum": ["sub", "main", "both", "event_boost", "off", "event_only"]
    });
    let stream_enum = json!({ "type": "string", "enum": ["sub", "main"] });
    json!({
        "type": "object",
        "properties": {
            "display_name": tristate_schema(json!({ "type": "string" })),
            "manufacturer": tristate_schema(json!({ "type": "string" })),
            "username_secret_reference": tristate_schema(secret_ref_string_schema()),
            "password_secret_reference": tristate_schema(secret_ref_string_schema()),
            "onvif_port": tristate_schema(json!({ "type": "integer", "minimum": 1, "maximum": 65535 })),
            "http_port": tristate_schema(json!({ "type": "integer", "minimum": 1, "maximum": 65535 })),
            "main_rtsp_url": tristate_schema(json!({ "type": "string" })),
            "sub_rtsp_url": tristate_schema(json!({ "type": "string" })),
            "uid_secret_reference": tristate_schema(secret_ref_string_schema()),
            "backend": tristate_schema(backend_enum),
            "transport": tristate_schema(transport_enum),
            "record_generic_motion_events": tristate_schema(json!({ "type": "boolean" })),
            "recording_mode": tristate_schema(mode_enum),
            "event_recording_duration_secs": tristate_schema(json!({ "type": "integer", "minimum": 1 })),
            "event_pre_recording_duration_secs": tristate_schema(json!({ "type": "integer", "minimum": 0 })),
            "event_recording_stream": tristate_schema(stream_enum)
        },
        "additionalProperties": false
    })
}

fn camera_defaults_patch_schema() -> Value {
    let backend_enum = json!({ "type": "string", "enum": ["auto", "retina", "reo_proto"] });
    let transport_enum = json!({ "type": "string", "enum": ["tcp", "udp"] });
    let mode_enum = json!({
        "type": "string",
        "enum": ["sub", "main", "both", "event_boost", "off", "event_only"]
    });
    let stream_enum = json!({ "type": "string", "enum": ["sub", "main"] });
    json!({
        "type": "object",
        "properties": {
            "username_secret_reference": tristate_schema(secret_ref_string_schema()),
            "password_secret_reference": tristate_schema(secret_ref_string_schema()),
            "backend": tristate_schema(backend_enum),
            "transport": tristate_schema(transport_enum),
            "record_generic_motion_events": tristate_schema(json!({ "type": "boolean" })),
            "recording_mode": tristate_schema(mode_enum),
            "event_recording_duration_secs": tristate_schema(json!({ "type": "integer", "minimum": 1 })),
            "event_pre_recording_duration_secs": tristate_schema(json!({ "type": "integer", "minimum": 0 })),
            "event_recording_stream": tristate_schema(stream_enum)
        },
        "additionalProperties": false
    })
}

fn update_runtime_configuration_schema() -> Value {
    json!({
        "type": "object",
        "required": ["expected_configuration_revision"],
        "properties": {
            "expected_configuration_revision": { "type": "string", "minLength": 1 },
            "host": { "type": "string" },
            "port": { "type": "integer", "minimum": 1, "maximum": 65535 },
            "move_existing_recordings": { "type": "boolean" },
            "storage": {
                "type": "object",
                "properties": {
                    "medium_term_path": { "type": "string" },
                    "long_term_path": { "type": "string" },
                    "recording_catalog_path": { "type": "string" },
                    "event_thumbnail_path": { "type": "string" },
                    "event_thumbnail_max_mb": { "type": "integer", "minimum": 1 },
                    "short_term_secs": { "type": "integer", "minimum": 1 },
                    "medium_term_secs": { "type": "integer", "minimum": 1 },
                    "flush_interval_secs": { "type": "integer", "minimum": 1 },
                    "write_buffer_bytes": { "type": "integer", "minimum": 1 },
                    "long_term_max_gb": { "type": "integer", "minimum": 0 },
                    "minimum_free_gb": { "type": "integer", "minimum": 0 },
                    "maximum_used_percent": { "type": "integer", "minimum": 0, "maximum": 99 },
                    "warning_free_gb": { "type": "integer", "minimum": 0 },
                    "critical_free_gb": { "type": "integer", "minimum": 0 },
                    "cleanup_hysteresis_gb": { "type": "integer", "minimum": 0 },
                    "pre_recording_stream_max_bytes": { "type": "integer", "minimum": 1 },
                    "pre_recording_global_max_bytes": { "type": "integer", "minimum": 1 }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

fn manage_template_schema() -> Value {
    json!({
        "type": "object",
        "required": ["action", "expected_configuration_revision"],
        "properties": {
            "action": { "type": "string", "enum": ["save", "duplicate", "delete"] },
            "expected_configuration_revision": { "type": "string", "minLength": 1 },
            "template_id": { "type": "string" },
            "name": { "type": "string" },
            "expected_template_version": { "type": "integer", "minimum": 0 },
            "template": {
                "type": "object",
                "required": ["name"],
                "properties": {
                    "template_id": { "type": "string" },
                    "name": { "type": "string", "minLength": 1 },
                    "description": { "type": "string" },
                    "values": {
                        "type": "object",
                        "properties": {
                            "username_secret_reference": secret_ref_string_schema(),
                            "password_secret_reference": secret_ref_string_schema(),
                            "onvif_port": { "type": "integer", "minimum": 1, "maximum": 65535 },
                            "http_port": { "type": "integer", "minimum": 1, "maximum": 65535 },
                            "backend": { "type": "string", "enum": ["auto", "retina", "reo_proto"] },
                            "transport": { "type": "string", "enum": ["tcp", "udp"] },
                            "record_generic_motion_events": { "type": "boolean" },
                            "recording_mode": {
                                "type": "string",
                                "enum": ["sub", "main", "both", "event_boost", "off", "event_only"]
                            },
                            "event_recording_duration_secs": { "type": "integer", "minimum": 1 },
                            "event_pre_recording_duration_secs": { "type": "integer", "minimum": 0 },
                            "event_recording_stream": { "type": "string", "enum": ["sub", "main"] }
                        },
                        "additionalProperties": false
                    }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

fn manage_camera_schema() -> Value {
    json!({
        "type": "object",
        "required": ["action", "ip", "expected_configuration_revision"],
        "properties": {
            "action": { "type": "string", "enum": ["upsert", "remove"] },
            "ip": { "type": "string", "description": "Camera IPv4 or IPv6 address." },
            "expected_configuration_revision": { "type": "string", "minLength": 1 },
            "display_name": { "type": ["string", "null"] },
            "manufacturer": { "type": ["string", "null"] },
            "username": secret_ref_string_schema(),
            "password": secret_ref_string_schema(),
            "uid": {
                "oneOf": [
                    secret_ref_string_schema(),
                    { "type": "null" }
                ]
            },
            "onvif_port": { "type": ["integer", "null"], "minimum": 1, "maximum": 65535 },
            "http_port": { "type": ["integer", "null"], "minimum": 1, "maximum": 65535 },
            "main_rtsp_url": {
                "type": ["string", "null"],
                "description": "RTSP URL without embedded credentials; configure username and password with {secret:KEY}."
            },
            "sub_rtsp_url": {
                "type": ["string", "null"],
                "description": "RTSP URL without embedded credentials; configure username and password with {secret:KEY}."
            },
            "backend": { "type": "string", "enum": ["auto", "retina", "reo_proto"] },
            "transport": { "type": "string", "enum": ["tcp", "udp"] },
            "record_generic_motion_events": { "type": "boolean" },
            "recording_mode": {
                "type": "string",
                "enum": ["sub", "main", "both", "event_boost", "off", "event_only"]
            },
            "event_recording_duration_secs": { "type": "integer", "minimum": 1 },
            "event_pre_recording_duration_secs": { "type": "integer", "minimum": 0 },
            "event_recording_stream": { "type": "string", "enum": ["sub", "main"] }
        },
        "additionalProperties": false
    })
}

fn manage_notification_rule_schema() -> Value {
    let definition = notification_rule_definition_schema();
    json!({
        "type": "object",
        "required": ["action"],
        "properties": {
            "action": { "type": "string", "enum": ["list", "save_draft", "activate", "delete"] },
            "rule_id": { "type": "string", "minLength": 1, "maxLength": 128 },
            "expected_active_revision": { "type": "integer", "minimum": 0 },
            "expected_draft_revision": { "type": "integer", "minimum": 0 },
            "definition": definition
        },
        "additionalProperties": false
    })
}

fn notification_rule_definition_schema() -> Value {
    let action_item = notification_action_schema();
    json!({
        "type": "object",
        "description": "Notification rule JSON object for save_draft. Action destinations for push/webhook/forwarder must use {secret:KEY} references or set destination_configured=true with destination_ref; raw credentials are rejected.",
        "required": ["id", "name", "enabled", "revision", "triggers", "schedule", "enrichment", "actions", "failure"],
        "properties": {
            "id": { "type": "string", "minLength": 1, "maxLength": 128 },
            "name": { "type": "string", "minLength": 1, "maxLength": 128 },
            "enabled": { "type": "boolean" },
            "revision": { "type": "integer", "minimum": 0 },
            "owner_id": { "type": "string" },
            "triggers": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "string",
                    "enum": ["event_created", "event_updated", "event_ended", "outage_started", "recovery", "storage_health", "recording_health", "test"]
                }
            },
            "filter": { "type": "object" },
            "schedule": {
                "type": "object",
                "required": ["timezone"],
                "properties": {
                    "timezone": { "type": "string" },
                    "active_windows": { "type": "array", "items": { "type": "object" } },
                    "quiet_hours": { "type": ["object", "null"] }
                }
            },
            "cooldowns": { "type": "array", "items": { "type": "object" } },
            "rate_limits": { "type": "array", "items": { "type": "object" } },
            "critical_bypass": { "type": ["object", "null"] },
            "enrichment": {
                "type": "object",
                "required": ["deadline_ms", "maximum_revisions", "maximum_attempts", "maximum_attachment_bytes", "wake_after_deadline"]
            },
            "actions": {
                "type": "array",
                "minItems": 1,
                "maxItems": 8,
                "items": action_item
            },
            "failure": {
                "type": "object",
                "required": ["maximum_attempts", "maximum_retry_interval_ms", "expiry_ms"]
            }
        }
    })
}

fn notification_action_schema() -> Value {
    json!({
        "type": "object",
        "required": ["channel", "destination", "template", "attachment", "allow_second_delivery"],
        "properties": {
            "enabled": { "type": "boolean" },
            "channel": { "type": "string", "enum": ["browser", "push", "webhook", "forwarder", "keeppeek_service"] },
            "destination": { "type": "string" },
            "destination_configured": { "type": "boolean" },
            "destination_ref": { "type": "string" },
            "template": {
                "type": "object",
                "required": ["title", "body"],
                "properties": {
                    "title": { "type": "string" },
                    "body": { "type": "string" }
                }
            },
            "attachment": { "type": "string", "enum": ["never", "when_available", "required"] },
            "allow_second_delivery": { "type": "boolean" }
        }
    })
}
