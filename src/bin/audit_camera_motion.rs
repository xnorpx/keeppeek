use clap::Parser;
use keeppeek::{cameras::reolink::ReolinkClient, config};
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

const AI_TYPES: &[&str] = &["people", "vehicle", "dog_cat", "face", "package"];

#[derive(Debug, Parser)]
#[command(
    name = "audit-camera-motion",
    about = "Read motion and AI alarm settings without changing camera configuration"
)]
struct Cli {
    /// KeepPeek camera configuration containing locally stored credentials.
    #[arg(long, default_value_os_t = config::config_path())]
    config: PathBuf,

    /// Safe JSON report path.
    #[arg(long, default_value = "target/camera-setup/motion-audit.json")]
    output: PathBuf,
}

#[derive(Serialize)]
struct CameraAudit {
    camera: String,
    display_name: String,
    ip: String,
    model: Option<String>,
    audited: bool,
    motion_active_now: Option<bool>,
    motion: DetectionSetting,
    ai: Vec<AiSetting>,
}

#[derive(Serialize)]
struct AiSetting {
    kind: String,
    #[serde(flatten)]
    setting: DetectionSetting,
}

#[derive(Serialize, Default)]
struct DetectionSetting {
    supported: bool,
    enabled: Option<bool>,
    sensitivity: Option<i64>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let configured = config::load_cameras(&cli.config)?;
    let mut cameras = configured.into_values().flatten().collect::<Vec<_>>();
    cameras.sort_unstable_by_key(|camera| camera.name.clone());
    let mut audits = Vec::with_capacity(cameras.len());

    for camera in cameras {
        let stable_name = camera.name.clone().unwrap_or_else(|| camera.ip.to_string());
        let display_name = camera.display_name().unwrap_or(&stable_name).to_owned();
        let mut client = ReolinkClient::new(camera.ip);
        if client.login(&camera.username, &camera.password).is_err() {
            audits.push(CameraAudit {
                camera: stable_name,
                display_name,
                ip: camera.ip.to_string(),
                model: None,
                audited: false,
                motion_active_now: None,
                motion: DetectionSetting::default(),
                ai: Vec::new(),
            });
            continue;
        }

        let model = client.get_dev_info().ok().and_then(|info| info.model);
        let motion = merge_settings(
            client.get_alarm(0).ok().as_ref(),
            client.get_md_alarm(0).ok().as_ref(),
        );
        let ai = AI_TYPES
            .iter()
            .map(|kind| AiSetting {
                kind: (*kind).to_owned(),
                setting: client
                    .get_ai_alarm(0, kind)
                    .ok()
                    .as_ref()
                    .map_or_else(DetectionSetting::default, inspect_setting),
            })
            .collect();
        let motion_active_now = client.get_md_state(0).ok();
        let _ = client.logout();
        audits.push(CameraAudit {
            camera: stable_name,
            display_name,
            ip: camera.ip.to_string(),
            model,
            audited: true,
            motion_active_now,
            motion,
            ai,
        });
    }

    if let Some(parent) = cli.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&cli.output, serde_json::to_vec_pretty(&audits)?)?;
    println!(
        "MOTION_AUDIT_OK file={} cameras={} audited={}",
        cli.output.display(),
        audits.len(),
        audits.iter().filter(|audit| audit.audited).count()
    );
    Ok(())
}

fn merge_settings(first: Option<&Value>, second: Option<&Value>) -> DetectionSetting {
    match (first, second) {
        (None, None) => DetectionSetting::default(),
        (first, second) => DetectionSetting {
            supported: true,
            enabled: first
                .and_then(|value| find_bool(value, &["enable", "enabled"]))
                .or_else(|| second.and_then(|value| find_bool(value, &["enable", "enabled"]))),
            sensitivity: first
                .and_then(|value| find_integer(value, &["sensDef", "sensitivity"]))
                .or_else(|| {
                    second.and_then(|value| find_integer(value, &["sensDef", "sensitivity"]))
                }),
        },
    }
}

fn inspect_setting(value: &Value) -> DetectionSetting {
    DetectionSetting {
        supported: true,
        enabled: find_bool(value, &["enable", "enabled"]),
        sensitivity: find_integer(value, &["sensDef", "sensitivity"]),
    }
}

fn find_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    find_scalar(value, keys).and_then(|value| match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => value.as_i64().map(|value| value != 0),
        Value::String(value) if value == "1" || value.eq_ignore_ascii_case("true") => Some(true),
        Value::String(value) if value == "0" || value.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    })
}

fn find_integer(value: &Value, keys: &[&str]) -> Option<i64> {
    find_scalar(value, keys).and_then(|value| match value {
        Value::Number(value) => value.as_i64(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    })
}

fn find_scalar<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(value) = object.iter().find_map(|(candidate, value)| {
                    candidate.eq_ignore_ascii_case(key).then_some(value)
                }) {
                    return Some(value);
                }
            }
            object.values().find_map(|value| find_scalar(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_scalar(value, keys)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_config_is_the_default() {
        let cli = Cli::try_parse_from(["audit-camera-motion"]).unwrap();

        assert_eq!(cli.config, config::config_path());
    }

    #[test]
    fn extracts_safe_motion_fields_from_nested_responses() {
        let value = serde_json::json!({
            "Alarm": {
                "enable": 1,
                "newSens": { "sensDef": 41 }
            }
        });

        let setting = inspect_setting(&value);
        assert!(setting.supported);
        assert_eq!(setting.enabled, Some(true));
        assert_eq!(setting.sensitivity, Some(41));
    }
}
