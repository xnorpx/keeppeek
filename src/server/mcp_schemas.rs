//! Complete MCP JSON Schema definitions for all 28 KeepPeek MCP tools.
//!
//! Every tool declares explicit parameter properties, types, bounds, enums, and
//! secret-reference constraints so LLM clients can construct valid requests
//! without guessing hidden field names.

use crate::mcp::protocol::{ToolAnnotations, ToolDefinition};
use serde_json::{Value, json};

const SECRET_REF_PATTERN: &str = r"^\{secret:[A-Z_][A-Z0-9_]*(\|url)?\}$";
const SECRET_KEY_PATTERN: &str = r"^[A-Z_][A-Z0-9_]*$";

/// Returns the tool definitions visible to the caller's role.
pub(super) fn tool_definitions(is_admin: bool) -> Vec<ToolDefinition> {
    let mut tools = user_tool_definitions();
    if !is_admin {
        return tools;
    }
    tools.extend(admin_read_tool_definitions());
    tools.extend(admin_diagnostics_tool_definitions());
    tools.extend(admin_onboarding_tool_definitions());
    tools.extend(admin_camera_debug_tool_definitions());
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

fn admin_onboarding_tool_definitions() -> Vec<ToolDefinition> {
    let open_world_read = ToolAnnotations {
        read_only_hint: true,
        destructive_hint: false,
        idempotent_hint: true,
        open_world_hint: true,
    };
    let secret_request_annotations = ToolAnnotations {
        read_only_hint: false,
        destructive_hint: false,
        idempotent_hint: false,
        open_world_hint: false,
    };
    let device_write_annotations = ToolAnnotations {
        read_only_hint: false,
        destructive_hint: true,
        idempotent_hint: false,
        open_world_hint: true,
    };
    vec![
        tool_def(
            "search_camera_catalog",
            "Search the embedded camera catalog by brand or model and optionally attach credential-free RTSP stream hints for a camera IP.",
            search_camera_catalog_schema(),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "probe_camera_streams",
            "Probe ONVIF and RTSP camera streams using single {secret:KEY} references or up to 5 credential_candidates; stops on first auth match or lockout.",
            probe_camera_streams_schema(),
            open_world_read,
        ),
        tool_def(
            "request_secret_entry",
            "Create a bounded single-use loopback secret entry handoff link and CLI commands without passing secret values through MCP.",
            request_secret_entry_schema(),
            secret_request_annotations,
        ),
        tool_def(
            "get_secret_entry_status",
            "Check the value-free status and configured key names of a pending or completed secret entry handoff.",
            single_string_arg_schema("handoff_id", 128),
            ToolAnnotations::read_only(),
        ),
        tool_def(
            "get_camera_device_settings",
            "Inspect live on-camera hardware settings and supported sections via Reolink, Hikvision ISAPI, or ONVIF.",
            single_string_arg_schema("camera_id", 256),
            open_world_read,
        ),
        tool_def(
            "update_camera_device_settings",
            "Preview (dry_run defaults to true) or apply bounded on-camera hardware setting updates when [mcp].allow_camera_device_writes is true; excludes firmware, factory reset, password, and network changes.",
            update_camera_device_settings_schema(),
            device_write_annotations,
        ),
    ]
}

fn admin_camera_debug_tool_definitions() -> Vec<ToolDefinition> {
    let camera_debug_annotations = ToolAnnotations {
        read_only_hint: false,
        destructive_hint: true,
        idempotent_hint: false,
        open_world_hint: true,
    };
    vec![tool_def(
        "camera_debugging",
        "WARNING: Administrator-only raw camera protocol debugging gated by [mcp].allow_camera_debugging = true. Grants full raw access to live video bitstreams, audio streams, decrypted network captures (PCAPNG), and camera credentials for configured or debug_camera_ids-scoped cameras.",
        camera_debugging_schema(),
        camera_debug_annotations,
    )]
}

fn camera_debugging_schema() -> Value {
    let mut properties = serde_json::Map::new();
    insert_camera_debug_target_properties(&mut properties);
    insert_camera_debug_option_properties(&mut properties);
    json!({
        "type": "object",
        "required": ["action"],
        "properties": properties,
        "additionalProperties": false
    })
}

fn insert_camera_debug_target_properties(properties: &mut serde_json::Map<String, Value>) {
    let entries = [
        (
            "action",
            json!({
                "type": "string",
                "enum": [
                    "inspect_pipeline",
                    "capture",
                    "compare_transports",
                    "export_repro_fixture",
                    "list_captures",
                    "delete_capture"
                ]
            }),
        ),
        (
            "camera_id",
            json!({ "type": "string", "minLength": 1, "maxLength": 128 }),
        ),
        (
            "capture_id",
            json!({ "type": "string", "minLength": 1, "maxLength": 128 }),
        ),
        (
            "stream",
            json!({ "type": "string", "enum": ["main", "sub", "both"] }),
        ),
        (
            "artifacts",
            json!({
                "type": "array",
                "minItems": 1,
                "maxItems": 5,
                "items": {
                    "type": "string",
                    "enum": [
                        "pcapng",
                        "video_bitstream",
                        "audio_bitstream",
                        "muxed_mp4",
                        "protocol_transcript"
                    ]
                }
            }),
        ),
        (
            "protocols",
            json!({
                "type": "array",
                "minItems": 1,
                "maxItems": 6,
                "items": {
                    "type": "string",
                    "enum": [
                        "rtsp",
                        "audio",
                        "onvif",
                        "isapi",
                        "reolink_http",
                        "reo_proto"
                    ]
                }
            }),
        ),
    ];
    for (key, value) in entries {
        properties.insert(key.to_owned(), value);
    }
}

fn insert_camera_debug_option_properties(properties: &mut serde_json::Map<String, Value>) {
    let entries = [
        (
            "duration_seconds",
            json!({ "type": "integer", "minimum": 1, "maximum": 60 }),
        ),
        (
            "max_bytes",
            json!({ "type": "integer", "minimum": 65536, "maximum": 268435456 }),
        ),
        (
            "include_raw_auth",
            json!({
                "type": "boolean",
                "description": "Defaults to true for raw debugging actions (capture, compare_transports); set false to redact credentials."
            }),
        ),
        (
            "trigger_probe",
            json!({
                "type": "boolean",
                "description": "Defaults to false. Set true to trigger an active protocol probe during capture."
            }),
        ),
        (
            "mode",
            json!({
                "type": "string",
                "enum": ["media_transports", "event_channels"],
                "description": "Comparison mode for compare_transports."
            }),
        ),
        (
            "strip_credentials",
            json!({
                "type": "boolean",
                "description": "Defaults to true for export_repro_fixture. Set false only when unredacted credentials in the fixture are explicitly intended."
            }),
        ),
        (
            "anonymize_network",
            json!({
                "type": "boolean",
                "description": "Defaults to true for export_repro_fixture to anonymize IP addresses and host identifiers."
            }),
        ),
        (
            "trim_media",
            json!({
                "type": "string",
                "enum": [
                    "headers_and_minimal_failing_slice",
                    "synthetic_payloads",
                    "keep_raw_frames"
                ]
            }),
        ),
        ("delete_all", json!({ "type": "boolean" })),
    ];
    for (key, value) in entries {
        properties.insert(key.to_owned(), value);
    }
}

fn single_string_arg_schema(field: &str, max_length: u64) -> Value {
    json!({
        "type": "object",
        "required": [field],
        "properties": {
            field: { "type": "string", "minLength": 1, "maxLength": max_length }
        },
        "additionalProperties": false
    })
}

fn search_camera_catalog_schema() -> Value {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": { "type": "string", "minLength": 1, "maxLength": 128 },
            "ip": { "type": "string", "description": "Optional camera IPv4 or IPv6 address for credential-free RTSP stream hints." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 25 }
        },
        "additionalProperties": false
    })
}

fn probe_camera_streams_schema() -> Value {
    let candidate = json!({
        "type": "object",
        "required": ["username_secret_reference", "password_secret_reference"],
        "properties": {
            "username_secret_reference": secret_ref_string_schema(),
            "password_secret_reference": secret_ref_string_schema(),
            "uid_secret_reference": secret_ref_string_schema()
        },
        "additionalProperties": false
    });
    json!({
        "type": "object",
        "required": ["ip"],
        "properties": {
            "ip": { "type": "string", "description": "Private-network or configured camera IPv4 or IPv6 address." },
            "onvif_port": { "type": "integer", "minimum": 1, "maximum": 65535 },
            "main_rtsp_url": { "type": "string", "description": "Optional RTSP URL without embedded credentials." },
            "sub_rtsp_url": { "type": "string", "description": "Optional RTSP URL without embedded credentials." },
            "transport": { "type": "string", "enum": ["tcp", "udp"] },
            "query_onvif": { "type": "boolean" },
            "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 15 },
            "username_secret_reference": secret_ref_string_schema(),
            "password_secret_reference": secret_ref_string_schema(),
            "uid_secret_reference": secret_ref_string_schema(),
            "credential_candidates": {
                "type": "array",
                "minItems": 1,
                "maxItems": 5,
                "items": candidate
            }
        },
        "additionalProperties": false
    })
}

fn request_secret_entry_schema() -> Value {
    json!({
        "type": "object",
        "required": ["keys"],
        "properties": {
            "keys": {
                "type": "array",
                "minItems": 1,
                "maxItems": 16,
                "items": {
                    "type": "object",
                    "required": ["key", "purpose"],
                    "properties": {
                        "key": { "type": "string", "minLength": 1, "maxLength": 128, "pattern": SECRET_KEY_PATTERN },
                        "purpose": { "type": "string", "minLength": 1, "maxLength": 160 }
                    },
                    "additionalProperties": false
                }
            },
            "ttl_seconds": { "type": "integer", "minimum": 60, "maximum": 300 }
        },
        "additionalProperties": false
    })
}

fn update_camera_device_settings_schema() -> Value {
    json!({
        "type": "object",
        "required": ["camera_id", "settings"],
        "properties": {
            "camera_id": { "type": "string", "minLength": 1, "maxLength": 256 },
            "dry_run": {
                "type": "boolean",
                "description": "Defaults to true. Set false only after explicit operator approval when [mcp].allow_camera_device_writes is true."
            },
            "settings": {
                "type": "object",
                "description": "Bounded hardware setting sections (encoder, imaging, lighting, motion, osd, time, ptz). Factory reset, reboot, firmware, credentials, and network changes are prohibited.",
                "properties": {
                    "encoder": { "type": "object" },
                    "imaging": { "type": "object" },
                    "lighting": { "type": "object" },
                    "motion": { "type": "object" },
                    "osd": { "type": "object" },
                    "time": { "type": "object" },
                    "ptz": { "type": "object" }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
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
        tool_def(
            "disable_mcp_access",
            "Immediately disable KeepPeek MCP server access ([mcp].enabled = false) and invalidate pending secret-entry handoffs. Permitted for Administrator callers even when [mcp].allow_writes is false (blocked under X-KeepPeek-MCP-Read-Only: true). Re-enabling MCP always requires a human Administrator in the Web UI or direct config.toml edit.",
            json!({
                "type": "object",
                "required": ["confirm"],
                "properties": {
                    "confirm": {
                        "type": "boolean",
                        "const": true,
                        "description": "Must be true to confirm disabling MCP access immediately."
                    },
                    "reason": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 256,
                        "description": "Optional short note explaining why the assistant disabled MCP access."
                    }
                },
                "additionalProperties": false
            }),
            ToolAnnotations::write(true, true),
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
            "expected_active_revision": {
                "type": "integer",
                "minimum": 0,
                "description": "Optimistic concurrency revision of the active rule (0 matches only an unactivated rule; never skips conflict checks)."
            },
            "expected_draft_revision": {
                "type": "integer",
                "minimum": 0,
                "description": "Optimistic concurrency revision of the draft rule (0 matches only a brand-new rule; never skips conflict checks)."
            },
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
