use clap::{Parser, ValueEnum};
use keeppeek::{cameras::CameraStreamSelection, config};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    path::{Path, PathBuf},
};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum VerifiedMode {
    ReoProtoTcp,
    RetinaTcp,
    RetinaUdp,
}

impl VerifiedMode {
    const fn values(self) -> (&'static str, &'static str) {
        match self {
            Self::ReoProtoTcp => ("reo-proto", "tcp"),
            Self::RetinaTcp => ("retina", "tcp"),
            Self::RetinaUdp => ("retina", "udp"),
        }
    }
}

#[derive(Debug, Clone)]
struct VerifiedCamera {
    ip: IpAddr,
    mode: VerifiedMode,
}

#[derive(Debug, Clone)]
struct DisplayName {
    camera: String,
    label: String,
}

#[derive(Debug, Clone)]
struct RtspEndpoint {
    camera: String,
    url: String,
}

#[derive(Debug, Clone)]
struct StreamSelection {
    camera: String,
    streams: CameraStreamSelection,
}

fn parse_display_name(value: &str) -> Result<DisplayName, String> {
    let (camera, label) = value
        .split_once('=')
        .ok_or_else(|| "expected CAMERA=LABEL".to_owned())?;
    let camera = camera.trim();
    let label = label.trim();
    if camera.is_empty() || label.is_empty() {
        return Err("camera and label must not be empty".to_owned());
    }
    if label.chars().any(char::is_control) {
        return Err("label must not contain control characters".to_owned());
    }
    Ok(DisplayName {
        camera: camera.to_owned(),
        label: label.to_owned(),
    })
}

fn parse_verified(value: &str) -> Result<VerifiedCamera, String> {
    let (ip, mode) = value
        .split_once('=')
        .ok_or_else(|| "expected IP=MODE".to_owned())?;
    let ip = ip
        .parse()
        .map_err(|_| format!("invalid camera IP '{ip}'"))?;
    let mode = match mode {
        "reo-proto-tcp" => VerifiedMode::ReoProtoTcp,
        "retina-tcp" => VerifiedMode::RetinaTcp,
        "retina-udp" => VerifiedMode::RetinaUdp,
        _ => return Err(format!("invalid mode '{mode}'")),
    };
    Ok(VerifiedCamera { ip, mode })
}

fn parse_rtsp_endpoint(value: &str) -> Result<RtspEndpoint, String> {
    let (camera, value) = value
        .split_once('=')
        .ok_or_else(|| "expected CAMERA=RTSP_URL".to_owned())?;
    let camera = camera.trim();
    let value = value.trim();
    if camera.is_empty() || value.is_empty() {
        return Err("camera and RTSP URL must not be empty".to_owned());
    }
    let url = Url::parse(value).map_err(|_| "RTSP URL is invalid".to_owned())?;
    if !matches!(url.scheme(), "rtsp" | "rtsps") || url.host_str().is_none() {
        return Err("RTSP URL must use rtsp or rtsps and include a host".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("RTSP URL must use the camera's configured credentials".to_owned());
    }
    Ok(RtspEndpoint {
        camera: camera.to_owned(),
        url: value.to_owned(),
    })
}

fn parse_stream_selection(value: &str) -> Result<StreamSelection, String> {
    let (camera, streams) = value
        .split_once('=')
        .ok_or_else(|| "expected CAMERA=main, CAMERA=sub, or CAMERA=both".to_owned())?;
    let camera = camera.trim();
    if camera.is_empty() {
        return Err("camera must not be empty".to_owned());
    }
    let streams = match streams.trim() {
        "main" => CameraStreamSelection::Main,
        "sub" => CameraStreamSelection::Sub,
        "both" => CameraStreamSelection::Both,
        streams => return Err(format!("invalid stream selection '{streams}'")),
    };
    Ok(StreamSelection {
        camera: camera.to_owned(),
        streams,
    })
}

#[derive(Debug, Parser)]
#[command(
    name = "configure-cameras",
    about = "Atomically update stream settings and display labels in a KeepPeek config"
)]
struct Cli {
    /// Existing KeepPeek configuration to preserve and update.
    #[arg(long, default_value_os_t = config::config_path())]
    base: PathBuf,

    /// Staged discovery configuration containing candidate cameras.
    #[arg(long)]
    candidates: Option<PathBuf>,

    /// Validated camera selection as IP=reo-proto-tcp, IP=retina-tcp, or IP=retina-udp.
    #[arg(long = "verified", value_parser = parse_verified)]
    verified: Vec<VerifiedCamera>,

    /// Human-readable label as CAMERA=LABEL, where CAMERA is a config key or IP address.
    #[arg(long = "display-name", value_parser = parse_display_name)]
    display_names: Vec<DisplayName>,

    /// Persist a validated main stream endpoint as CAMERA=RTSP_URL.
    #[arg(long = "main-rtsp-url", value_parser = parse_rtsp_endpoint)]
    main_rtsp_urls: Vec<RtspEndpoint>,

    /// Persist a validated sub stream endpoint as CAMERA=RTSP_URL.
    #[arg(long = "sub-rtsp-url", value_parser = parse_rtsp_endpoint)]
    sub_rtsp_urls: Vec<RtspEndpoint>,

    /// Select video profiles as CAMERA=main, CAMERA=sub, or CAMERA=both.
    #[arg(long = "streams", value_parser = parse_stream_selection)]
    streams: Vec<StreamSelection>,

    /// Camera config key or IP address to remove.
    #[arg(long = "remove")]
    remove: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let base_text = std::fs::read_to_string(&cli.base)?;
    let mut base: toml::Table = toml::from_str(&base_text)?;
    let verified = verified_modes(&cli.verified)?;
    if verified.is_empty()
        && cli.display_names.is_empty()
        && cli.main_rtsp_urls.is_empty()
        && cli.sub_rtsp_urls.is_empty()
        && cli.streams.is_empty()
        && cli.remove.is_empty()
    {
        anyhow::bail!(
            "provide at least one --verified, --display-name, --main-rtsp-url, --sub-rtsp-url, --streams, or --remove update"
        );
    }

    let merged = if verified.is_empty() {
        MergeSummary::default()
    } else {
        let candidate_path = cli
            .candidates
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--candidates is required with --verified"))?;
        let candidates_text = std::fs::read_to_string(candidate_path)?;
        let candidates: toml::Table = toml::from_str(&candidates_text)?;
        merge_configs(&mut base, &candidates, &verified)?
    };
    let renamed = apply_display_names(&mut base, &cli.display_names)?;
    let rtsp_urls = apply_rtsp_urls(&mut base, "main_rtsp_url", &cli.main_rtsp_urls)?
        + apply_rtsp_urls(&mut base, "sub_rtsp_url", &cli.sub_rtsp_urls)?;
    let streams = apply_stream_selections(&mut base, &cli.streams)?;
    let removed = remove_cameras(&mut base, &cli.remove)?;
    atomic_write(&cli.base, toml::to_string_pretty(&base)?.as_bytes())?;
    println!(
        "CONFIGURE_CAMERAS_OK file={} verified={} added={} updated={} display_names={} rtsp_urls={} streams={} removed={}",
        cli.base.display(),
        verified.len(),
        merged.added,
        merged.updated,
        renamed,
        rtsp_urls,
        streams,
        removed,
    );
    Ok(())
}

fn verified_modes(cameras: &[VerifiedCamera]) -> anyhow::Result<BTreeMap<IpAddr, VerifiedMode>> {
    let mut verified = BTreeMap::new();
    for camera in cameras {
        if let Some(previous) = verified.insert(camera.ip, camera.mode)
            && previous != camera.mode
        {
            anyhow::bail!("camera {} has conflicting verified modes", camera.ip);
        }
    }
    Ok(verified)
}

#[derive(Default)]
struct MergeSummary {
    added: usize,
    updated: usize,
}

fn apply_display_names(base: &mut toml::Table, names: &[DisplayName]) -> anyhow::Result<usize> {
    let cameras = base
        .get_mut("cameras")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("base config has no [cameras] table"))?;
    let mut labels = BTreeMap::new();
    let mut updates = Vec::new();
    for name in names {
        let key = if cameras.contains_key(&name.camera) {
            name.camera.clone()
        } else if let Ok(ip) = name.camera.parse() {
            find_camera(cameras, ip)
                .map(|(key, _)| key.to_owned())
                .ok_or_else(|| anyhow::anyhow!("camera '{}' was not found", name.camera))?
        } else {
            anyhow::bail!("camera '{}' was not found", name.camera);
        };
        if let Some(previous) = labels.insert(name.label.to_lowercase(), key.clone())
            && previous != key
        {
            anyhow::bail!("display label '{}' is assigned more than once", name.label);
        }
        updates.push((key, name.label.clone()));
    }
    for (key, label) in &updates {
        cameras[key]
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("camera '{key}' is not a table"))?
            .insert(
                "display_name".to_owned(),
                toml::Value::String(label.clone()),
            );
    }
    Ok(updates.len())
}

fn apply_rtsp_urls(
    base: &mut toml::Table,
    key: &str,
    endpoints: &[RtspEndpoint],
) -> anyhow::Result<usize> {
    let cameras = base
        .get_mut("cameras")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("base config has no [cameras] table"))?;
    let mut updates = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        let camera = if cameras.contains_key(&endpoint.camera) {
            endpoint.camera.clone()
        } else if let Ok(ip) = endpoint.camera.parse() {
            find_camera(cameras, ip)
                .map(|(camera, _)| camera.to_owned())
                .ok_or_else(|| anyhow::anyhow!("camera '{}' was not found", endpoint.camera))?
        } else {
            anyhow::bail!("camera '{}' was not found", endpoint.camera);
        };
        updates.push((camera, endpoint.url.clone()));
    }
    for (camera, url) in updates {
        cameras[&camera]
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("camera '{camera}' is not a table"))?
            .insert(key.to_owned(), toml::Value::String(url));
    }
    Ok(endpoints.len())
}

fn apply_stream_selections(
    base: &mut toml::Table,
    selections: &[StreamSelection],
) -> anyhow::Result<usize> {
    let cameras = base
        .get_mut("cameras")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("base config has no [cameras] table"))?;
    let mut updates = Vec::with_capacity(selections.len());
    for selection in selections {
        let camera = if cameras.contains_key(&selection.camera) {
            selection.camera.clone()
        } else if let Ok(ip) = selection.camera.parse() {
            find_camera(cameras, ip)
                .map(|(camera, _)| camera.to_owned())
                .ok_or_else(|| anyhow::anyhow!("camera '{}' was not found", selection.camera))?
        } else {
            anyhow::bail!("camera '{}' was not found", selection.camera);
        };
        let streams = match selection.streams {
            CameraStreamSelection::Main => "main",
            CameraStreamSelection::Sub => "sub",
            CameraStreamSelection::Both => "both",
        };
        updates.push((camera, streams));
    }
    for (camera, streams) in updates {
        cameras[&camera]
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("camera '{camera}' is not a table"))?
            .insert(
                "streams".to_owned(),
                toml::Value::String(streams.to_owned()),
            );
    }
    Ok(selections.len())
}

fn remove_cameras(base: &mut toml::Table, removals: &[String]) -> anyhow::Result<usize> {
    let cameras = base
        .get_mut("cameras")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("base config has no [cameras] table"))?;
    let mut keys = BTreeSet::new();
    for camera in removals {
        let key = if cameras.contains_key(camera) {
            camera.clone()
        } else if let Ok(ip) = camera.parse() {
            find_camera(cameras, ip)
                .map(|(key, _)| key.to_owned())
                .ok_or_else(|| anyhow::anyhow!("camera '{camera}' was not found"))?
        } else {
            anyhow::bail!("camera '{camera}' was not found");
        };
        keys.insert(key);
    }
    for key in &keys {
        cameras.remove(key);
    }
    Ok(keys.len())
}

fn merge_configs(
    base: &mut toml::Table,
    candidates: &toml::Table,
    verified: &BTreeMap<IpAddr, VerifiedMode>,
) -> anyhow::Result<MergeSummary> {
    let candidate_cameras = candidates
        .get("cameras")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| anyhow::anyhow!("candidate config has no [cameras] table"))?;
    let base_cameras = base
        .entry("cameras")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("base [cameras] value is not a table"))?;

    let mut summary = MergeSummary {
        added: 0,
        updated: 0,
    };
    for (ip, mode) in verified {
        let (candidate_key, candidate_value) = find_camera(candidate_cameras, *ip)
            .ok_or_else(|| anyhow::anyhow!("verified camera {ip} is absent from candidates"))?;
        let (backend, transport) = mode.values();

        if let Some(existing_key) = find_camera(base_cameras, *ip).map(|(key, _)| key.to_owned()) {
            let existing = base_cameras[&existing_key]
                .as_table_mut()
                .expect("matched camera entry must remain a table");
            existing.insert(
                "backend".to_owned(),
                toml::Value::String(backend.to_owned()),
            );
            existing.insert(
                "transport".to_owned(),
                toml::Value::String(transport.to_owned()),
            );
            if let Some(onvif_port) = candidate_value
                .as_table()
                .and_then(|table| table.get("onvif_port"))
            {
                existing.insert("onvif_port".to_owned(), onvif_port.clone());
            }
            summary.updated += 1;
            continue;
        }

        let mut value = candidate_value.clone();
        let table = value
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("candidate camera '{candidate_key}' is not a table"))?;
        table.insert(
            "backend".to_owned(),
            toml::Value::String(backend.to_owned()),
        );
        table.insert(
            "transport".to_owned(),
            toml::Value::String(transport.to_owned()),
        );
        let key = unique_key(base_cameras, candidate_key);
        base_cameras.insert(key, value);
        summary.added += 1;
    }
    Ok(summary)
}

fn find_camera(table: &toml::Table, ip: IpAddr) -> Option<(&str, &toml::Value)> {
    table.iter().find_map(|(key, value)| {
        let candidate_ip = value
            .as_table()?
            .get("ip")?
            .as_str()?
            .parse::<IpAddr>()
            .ok()?;
        (candidate_ip == ip).then_some((key.as_str(), value))
    })
}

fn unique_key(table: &toml::Table, candidate: &str) -> String {
    if !table.contains_key(candidate) {
        return candidate.to_owned();
    }
    for suffix in 2.. {
        let key = format!("{candidate}_{suffix}");
        if !table.contains_key(&key) {
            return key;
        }
    }
    unreachable!("integer key suffixes must eventually be unique")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let temporary = path.with_file_name(format!(".{filename}.tmp"));
    let permissions = std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    if let Some(permissions) = permissions {
        std::fs::set_permissions(&temporary, permissions)?;
    }
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_config_is_the_default() {
        let cli = Cli::try_parse_from(["configure-cameras"]).unwrap();

        assert_eq!(cli.base, config::config_path());
    }

    #[test]
    fn merge_preserves_root_settings_and_existing_camera_identity() {
        let mut base: toml::Table = toml::from_str(
            r#"
                host = "0.0.0.0"
                [storage]
                medium_term_secs = 60
                [cameras.existing_label]
                ip = "192.168.1.10"
                username = "old-user"
                password = "old-secret"
                uid = "preserve-me"
                backend = "auto"
            "#,
        )
        .unwrap();
        let candidates: toml::Table = toml::from_str(
            r#"
                [cameras.new_label]
                ip = "192.168.1.10"
                username = "candidate-user"
                password = "candidate-secret"
                onvif_port = 8000
                [cameras.second]
                ip = "192.168.1.11"
                username = "new-user"
                password = "new-secret"
            "#,
        )
        .unwrap();
        let verified = BTreeMap::from([
            ("192.168.1.10".parse().unwrap(), VerifiedMode::ReoProtoTcp),
            ("192.168.1.11".parse().unwrap(), VerifiedMode::RetinaTcp),
        ]);

        let summary = merge_configs(&mut base, &candidates, &verified).unwrap();

        assert_eq!(summary.updated, 1);
        assert_eq!(summary.added, 1);
        assert_eq!(base["host"].as_str(), Some("0.0.0.0"));
        assert_eq!(base["storage"]["medium_term_secs"].as_integer(), Some(60));
        let cameras = base["cameras"].as_table().unwrap();
        let existing = cameras["existing_label"].as_table().unwrap();
        assert_eq!(existing["username"].as_str(), Some("old-user"));
        assert_eq!(existing["password"].as_str(), Some("old-secret"));
        assert_eq!(existing["uid"].as_str(), Some("preserve-me"));
        assert_eq!(existing["backend"].as_str(), Some("reo-proto"));
        assert_eq!(existing["transport"].as_str(), Some("tcp"));
        assert_eq!(existing["onvif_port"].as_integer(), Some(8000));
        assert_eq!(cameras["second"]["backend"].as_str(), Some("retina"));
    }

    #[test]
    fn display_names_preserve_camera_keys_and_credentials() {
        let mut base: toml::Table = toml::from_str(
            r#"
                [cameras.frontyardnorth]
                ip = "192.168.1.10"
                username = "operator"
                password = "secret"
            "#,
        )
        .unwrap();
        let names = vec![DisplayName {
            camera: "192.168.1.10".to_owned(),
            label: "North Garden".to_owned(),
        }];

        assert_eq!(apply_display_names(&mut base, &names).unwrap(), 1);
        let camera = base["cameras"]["frontyardnorth"].as_table().unwrap();
        assert_eq!(camera["display_name"].as_str(), Some("North Garden"));
        assert_eq!(camera["username"].as_str(), Some("operator"));
        assert_eq!(camera["password"].as_str(), Some("secret"));
    }

    #[test]
    fn rtsp_urls_preserve_camera_credentials_and_other_settings() {
        let mut base: toml::Table = toml::from_str(
            r#"
                host = "0.0.0.0"
                [cameras.deck]
                ip = "192.168.1.10"
                username = "operator"
                password = "secret"
                backend = "retina"
            "#,
        )
        .unwrap();
        let main = vec![parse_rtsp_endpoint("192.168.1.10=rtsp://192.168.1.10/main").unwrap()];
        let sub = vec![parse_rtsp_endpoint("deck=rtsp://192.168.1.10/sub").unwrap()];

        assert_eq!(
            apply_rtsp_urls(&mut base, "main_rtsp_url", &main).unwrap(),
            1
        );
        assert_eq!(apply_rtsp_urls(&mut base, "sub_rtsp_url", &sub).unwrap(), 1);

        let camera = base["cameras"]["deck"].as_table().unwrap();
        assert_eq!(camera["username"].as_str(), Some("operator"));
        assert_eq!(camera["password"].as_str(), Some("secret"));
        assert_eq!(camera["backend"].as_str(), Some("retina"));
        assert_eq!(
            camera["main_rtsp_url"].as_str(),
            Some("rtsp://192.168.1.10/main")
        );
        assert_eq!(
            camera["sub_rtsp_url"].as_str(),
            Some("rtsp://192.168.1.10/sub")
        );
        assert_eq!(base["host"].as_str(), Some("0.0.0.0"));
    }

    #[test]
    fn rtsp_url_rejects_embedded_credentials() {
        assert!(parse_rtsp_endpoint("deck=rtsp://operator:secret@192.168.1.10/main").is_err());
    }

    #[test]
    fn stream_selection_preserves_credentials_and_other_settings() {
        let mut base: toml::Table = toml::from_str(
            r#"
                [cameras.tapo]
                ip = "192.168.1.10"
                username = "operator"
                password = "secret"
                transport = "udp"
            "#,
        )
        .unwrap();
        let selections = vec![parse_stream_selection("192.168.1.10=main").unwrap()];

        assert_eq!(apply_stream_selections(&mut base, &selections).unwrap(), 1);
        let camera = base["cameras"]["tapo"].as_table().unwrap();
        assert_eq!(camera["streams"].as_str(), Some("main"));
        assert_eq!(camera["username"].as_str(), Some("operator"));
        assert_eq!(camera["password"].as_str(), Some("secret"));
        assert_eq!(camera["transport"].as_str(), Some("udp"));
    }

    #[test]
    fn remove_camera_preserves_other_cameras_and_root_settings() {
        let mut base: toml::Table = toml::from_str(
            r#"
                host = "0.0.0.0"
                [cameras.dead]
                ip = "192.168.1.10"
                username = "operator"
                password = "secret"
                [cameras.online]
                ip = "192.168.1.11"
                username = "operator"
                password = "other-secret"
            "#,
        )
        .unwrap();

        assert_eq!(
            remove_cameras(&mut base, &["192.168.1.10".to_owned()]).unwrap(),
            1
        );
        assert_eq!(base["host"].as_str(), Some("0.0.0.0"));
        let cameras = base["cameras"].as_table().unwrap();
        assert!(!cameras.contains_key("dead"));
        assert_eq!(cameras["online"]["password"].as_str(), Some("other-secret"));
    }
}
