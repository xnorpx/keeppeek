//! Reviewed finite normalization mappings for privacy-preserving usage reporting.
//!
//! Every untrusted or user-controlled string maps into a closed enum or `"unknown"`.

use super::model::{
    Architecture, CpuModelFamily, CpuVendor, DeploymentType, MemoryBucket, OsFamily,
    ProjectedRetentionBucket, ReleaseChannel, ResolutionBucket, StorageCapacityBucket,
    TelemetryCameraBackend, TelemetryCameraTransport, TelemetryRecordingMode, VideoAcceleration,
    VideoCodec,
};
use crate::camera_database::{CameraDatabase, CameraMatch};

pub const UNKNOWN_SLUG: &str = "unknown";

pub const ALLOWED_MANUFACTURERS: &[&str] = &[
    "amcrest",
    "annke",
    "aqara",
    "axis",
    "bosch",
    "dahua",
    "eufy",
    "ezviz",
    "foscam",
    "hanwha",
    "hikvision",
    "imou",
    "lorex",
    "reolink",
    "sonoff",
    "swann",
    "tapo",
    "tp_link",
    "ubiquiti",
    "uniview",
    "vivotek",
    "wyze",
    "unknown",
];

// Each entry pairs an allowlisted manufacturer slug with an allowlisted canonical model slug.
pub const ALLOWED_MODELS: &[(&str, &str)] = &[
    ("amcrest", "amcrest_ad410"),
    ("amcrest", "amcrest_ip4m_1041"),
    ("amcrest", "amcrest_ip5m_t1179e"),
    ("amcrest", "amcrest_ip8m_2496e"),
    ("amcrest", "amcrest_ip8m_t2599e"),
    ("annke", "annke_c500"),
    ("annke", "annke_c800"),
    ("annke", "annke_ncd800"),
    ("aqara", "aqara_g2h_pro"),
    ("aqara", "aqara_g3"),
    ("aqara", "aqara_g4"),
    ("axis", "axis_m1065_lw"),
    ("axis", "axis_m2025_le"),
    ("axis", "axis_p1448_le"),
    ("axis", "axis_p3245_lv"),
    ("bosch", "bosch_flexidome_ip"),
    ("dahua", "dahua_ipc_hdw2431t"),
    ("dahua", "dahua_ipc_hdw5442t"),
    ("dahua", "dahua_ipc_hfw2831t"),
    ("dahua", "dahua_ipc_t5442t"),
    ("dahua", "dahua_vto2202f"),
    ("eufy", "eufy_indoor_cam_2k"),
    ("eufy", "eufy_indoor_cam_c120"),
    ("eufy", "eufy_s330"),
    ("ezviz", "ezviz_c3n"),
    ("ezviz", "ezviz_c6n"),
    ("ezviz", "ezviz_db1"),
    ("foscam", "foscam_c1"),
    ("foscam", "foscam_fi9821p"),
    ("foscam", "foscam_r2"),
    ("hanwha", "hanwha_qno_6012r"),
    ("hanwha", "hanwha_xno_6080r"),
    ("hikvision", "hikvision_ds_2cd2043g2"),
    ("hikvision", "hikvision_ds_2cd2087g2"),
    ("hikvision", "hikvision_ds_2cd2143g2"),
    ("hikvision", "hikvision_ds_2cd2343g2"),
    ("hikvision", "hikvision_ds_2cd2347g2"),
    ("hikvision", "hikvision_ds_2cd2387g2"),
    ("imou", "imou_bullet_2c"),
    ("imou", "imou_ranger_2"),
    ("lorex", "lorex_e891ab"),
    ("lorex", "lorex_lne8924ab"),
    ("reolink", "reolink_argus_3_pro"),
    ("reolink", "reolink_cx410"),
    ("reolink", "reolink_cx810"),
    ("reolink", "reolink_duo_2_poe"),
    ("reolink", "reolink_duo_3_poe"),
    ("reolink", "reolink_e1_outdoor"),
    ("reolink", "reolink_e1_pro"),
    ("reolink", "reolink_e1_zoom"),
    ("reolink", "reolink_rlc_410"),
    ("reolink", "reolink_rlc_510a"),
    ("reolink", "reolink_rlc_520a"),
    ("reolink", "reolink_rlc_810a"),
    ("reolink", "reolink_rlc_811a"),
    ("reolink", "reolink_rlc_820a"),
    ("reolink", "reolink_rlc_823a"),
    ("reolink", "reolink_rlc_1212a"),
    ("reolink", "reolink_rlc_1224a"),
    ("reolink", "reolink_trackmix_poe"),
    ("reolink", "reolink_video_doorbell_poe"),
    ("reolink", "reolink_video_doorbell_wifi"),
    ("sonoff", "sonoff_gk_200mp2_b"),
    ("swann", "swann_nht_865"),
    ("tapo", "tapo_c110"),
    ("tapo", "tapo_c200"),
    ("tapo", "tapo_c210"),
    ("tapo", "tapo_c310"),
    ("tapo", "tapo_c320ws"),
    ("tapo", "tapo_c520ws"),
    ("tp_link", "tp_link_vigi_c340"),
    ("tp_link", "tp_link_vigi_c440"),
    ("ubiquiti", "ubiquiti_g3_flex"),
    ("ubiquiti", "ubiquiti_g4_bullet"),
    ("ubiquiti", "ubiquiti_g4_doorbell_pro"),
    ("ubiquiti", "ubiquiti_g4_instant"),
    ("ubiquiti", "ubiquiti_g4_pro"),
    ("ubiquiti", "ubiquiti_g5_bullet"),
    ("ubiquiti", "ubiquiti_g5_dome"),
    ("ubiquiti", "ubiquiti_g5_flex"),
    ("uniview", "uniview_ipc2124lr3"),
    ("uniview", "uniview_ipc3614le"),
    ("vivotek", "vivotek_fd9165"),
    ("vivotek", "vivotek_ib9365"),
    ("wyze", "wyze_cam_v3"),
    ("wyze", "wyze_cam_v4"),
];

const MANUFACTURER_ALIASES: &[(&str, &str)] = &[
    ("amcrest", "amcrest"),
    ("amcresttechnologies", "amcrest"),
    ("annke", "annke"),
    ("aqara", "aqara"),
    ("lumi", "aqara"),
    ("axis", "axis"),
    ("axiscommunications", "axis"),
    ("bosch", "bosch"),
    ("boschsecuritysystems", "bosch"),
    ("dahua", "dahua"),
    ("dahuatechnology", "dahua"),
    ("zhejiangdahuatechnology", "dahua"),
    ("eufy", "eufy"),
    ("eufysecurity", "eufy"),
    ("anker", "eufy"),
    ("ezviz", "ezviz"),
    ("foscam", "foscam"),
    ("hanwha", "hanwha"),
    ("hanwhatechwin", "hanwha"),
    ("hanwhavision", "hanwha"),
    ("wisenet", "hanwha"),
    ("hikvision", "hikvision"),
    ("hangzhouhikvision", "hikvision"),
    ("imou", "imou"),
    ("lorex", "lorex"),
    ("reolink", "reolink"),
    ("sonoff", "sonoff"),
    ("itead", "sonoff"),
    ("swann", "swann"),
    ("tapo", "tapo"),
    ("tplink", "tp_link"),
    ("tplinkvigi", "tp_link"),
    ("ubiquiti", "ubiquiti"),
    ("ubiquitinetworks", "ubiquiti"),
    ("unifi", "ubiquiti"),
    ("unifiprotect", "ubiquiti"),
    ("uniview", "uniview"),
    ("unv", "uniview"),
    ("vivotek", "vivotek"),
    ("wyze", "wyze"),
    ("wyzelabs", "wyze"),
];

const MODEL_ALIASES: &[(&str, &str, &str)] = &[
    ("amcrest", "ad410", "amcrest_ad410"),
    ("amcrest", "ip4m1041", "amcrest_ip4m_1041"),
    ("amcrest", "ip5mt1179e", "amcrest_ip5m_t1179e"),
    ("amcrest", "ip8m2496e", "amcrest_ip8m_2496e"),
    ("amcrest", "ip8mt2599e", "amcrest_ip8m_t2599e"),
    ("annke", "c500", "annke_c500"),
    ("annke", "c800", "annke_c800"),
    ("annke", "ncd800", "annke_ncd800"),
    ("aqara", "g2hpro", "aqara_g2h_pro"),
    ("aqara", "g3", "aqara_g3"),
    ("aqara", "g4", "aqara_g4"),
    ("axis", "m1065lw", "axis_m1065_lw"),
    ("axis", "m2025le", "axis_m2025_le"),
    ("axis", "p1448le", "axis_p1448_le"),
    ("axis", "p3245lv", "axis_p3245_lv"),
    ("bosch", "flexidomeip", "bosch_flexidome_ip"),
    ("dahua", "ipchdw2431t", "dahua_ipc_hdw2431t"),
    ("dahua", "ipchdw5442t", "dahua_ipc_hdw5442t"),
    ("dahua", "ipchfw2831t", "dahua_ipc_hfw2831t"),
    ("dahua", "ipct5442t", "dahua_ipc_t5442t"),
    ("dahua", "vto2202f", "dahua_vto2202f"),
    ("eufy", "indoorcam2k", "eufy_indoor_cam_2k"),
    ("eufy", "indoorcamc120", "eufy_indoor_cam_c120"),
    ("eufy", "s330", "eufy_s330"),
    ("ezviz", "c3n", "ezviz_c3n"),
    ("ezviz", "c6n", "ezviz_c6n"),
    ("ezviz", "db1", "ezviz_db1"),
    ("foscam", "c1", "foscam_c1"),
    ("foscam", "fi9821p", "foscam_fi9821p"),
    ("foscam", "r2", "foscam_r2"),
    ("hanwha", "qno6012r", "hanwha_qno_6012r"),
    ("hanwha", "xno6080r", "hanwha_xno_6080r"),
    ("hikvision", "ds2cd2043g2", "hikvision_ds_2cd2043g2"),
    ("hikvision", "ds2cd2087g2", "hikvision_ds_2cd2087g2"),
    ("hikvision", "ds2cd2143g2", "hikvision_ds_2cd2143g2"),
    ("hikvision", "ds2cd2343g2", "hikvision_ds_2cd2343g2"),
    ("hikvision", "ds2cd2347g2", "hikvision_ds_2cd2347g2"),
    ("hikvision", "ds2cd2387g2", "hikvision_ds_2cd2387g2"),
    ("imou", "bullet2c", "imou_bullet_2c"),
    ("imou", "ranger2", "imou_ranger_2"),
    ("lorex", "e891ab", "lorex_e891ab"),
    ("lorex", "lne8924ab", "lorex_lne8924ab"),
    ("reolink", "argus3pro", "reolink_argus_3_pro"),
    ("reolink", "cx410", "reolink_cx410"),
    ("reolink", "cx810", "reolink_cx810"),
    ("reolink", "duo2poe", "reolink_duo_2_poe"),
    ("reolink", "duo3poe", "reolink_duo_3_poe"),
    ("reolink", "e1outdoor", "reolink_e1_outdoor"),
    ("reolink", "e1pro", "reolink_e1_pro"),
    ("reolink", "e1zoom", "reolink_e1_zoom"),
    ("reolink", "rlc410", "reolink_rlc_410"),
    ("reolink", "rlc510a", "reolink_rlc_510a"),
    ("reolink", "rlc520a", "reolink_rlc_520a"),
    ("reolink", "rlc810a", "reolink_rlc_810a"),
    ("reolink", "rlc811a", "reolink_rlc_811a"),
    ("reolink", "rlc820a", "reolink_rlc_820a"),
    ("reolink", "rlc823a", "reolink_rlc_823a"),
    ("reolink", "rlc1212a", "reolink_rlc_1212a"),
    ("reolink", "rlc1224a", "reolink_rlc_1224a"),
    ("reolink", "trackmixpoe", "reolink_trackmix_poe"),
    ("reolink", "videodoorbellpoe", "reolink_video_doorbell_poe"),
    (
        "reolink",
        "videodoorbellwifi",
        "reolink_video_doorbell_wifi",
    ),
    ("sonoff", "gk200mp2b", "sonoff_gk_200mp2_b"),
    ("swann", "nht865", "swann_nht_865"),
    ("tapo", "c110", "tapo_c110"),
    ("tapo", "c200", "tapo_c200"),
    ("tapo", "c210", "tapo_c210"),
    ("tapo", "c310", "tapo_c310"),
    ("tapo", "c320ws", "tapo_c320ws"),
    ("tapo", "c520ws", "tapo_c520ws"),
    ("tp_link", "vigic340", "tp_link_vigi_c340"),
    ("tp_link", "vigic440", "tp_link_vigi_c440"),
    ("ubiquiti", "g3flex", "ubiquiti_g3_flex"),
    ("ubiquiti", "g4bullet", "ubiquiti_g4_bullet"),
    ("ubiquiti", "g4doorbellpro", "ubiquiti_g4_doorbell_pro"),
    ("ubiquiti", "g4instant", "ubiquiti_g4_instant"),
    ("ubiquiti", "g4pro", "ubiquiti_g4_pro"),
    ("ubiquiti", "g5bullet", "ubiquiti_g5_bullet"),
    ("ubiquiti", "g5dome", "ubiquiti_g5_dome"),
    ("ubiquiti", "g5flex", "ubiquiti_g5_flex"),
    ("uniview", "ipc2124lr3", "uniview_ipc2124lr3"),
    ("uniview", "ipc3614le", "uniview_ipc3614le"),
    ("vivotek", "fd9165", "vivotek_fd9165"),
    ("vivotek", "ib9365", "vivotek_ib9365"),
    ("wyze", "camv3", "wyze_cam_v3"),
    ("wyze", "camv4", "wyze_cam_v4"),
];

pub fn is_allowed_manufacturer(value: &str) -> bool {
    ALLOWED_MANUFACTURERS.contains(&value)
}

pub fn is_allowed_model_for_manufacturer(manufacturer: &str, model: &str) -> bool {
    if model == UNKNOWN_SLUG {
        return true;
    }
    ALLOWED_MODELS.contains(&(manufacturer, model))
}

/// Normalizes a camera manufacturer and model into reviewed finite slugs or `"unknown"`.
pub fn normalize_camera_identity(
    database: Option<&CameraDatabase>,
    raw_manufacturer: Option<&str>,
    raw_model: Option<&str>,
) -> (&'static str, &'static str) {
    let (candidate_brand, candidate_model) = match (database, raw_manufacturer, raw_model) {
        (Some(db), brand, Some(model)) => match db.match_camera(brand.unwrap_or(""), model) {
            CameraMatch::Exact(matched) => (
                Some(matched.brand.as_str().to_owned()),
                Some(matched.model.as_str().to_owned()),
            ),
            CameraMatch::Ambiguous | CameraMatch::Missing => (
                raw_manufacturer.map(ToOwned::to_owned),
                raw_model.map(ToOwned::to_owned),
            ),
        },
        _ => (
            raw_manufacturer.map(ToOwned::to_owned),
            raw_model.map(ToOwned::to_owned),
        ),
    };

    let manufacturer = candidate_brand
        .as_deref()
        .map_or(UNKNOWN_SLUG, normalize_manufacturer_slug);
    let model = candidate_model.as_deref().map_or(UNKNOWN_SLUG, |value| {
        normalize_model_slug(manufacturer, value)
    });

    // Keep manufacturer and model consistent with the reviewed (manufacturer, model) allowlist.
    let manufacturer = if model != UNKNOWN_SLUG {
        ALLOWED_MODELS
            .iter()
            .find(|(_, allowed_model)| *allowed_model == model)
            .map_or(UNKNOWN_SLUG, |(brand, _)| *brand)
    } else {
        manufacturer
    };
    (manufacturer, model)
}

pub fn normalize_manufacturer_slug(raw: &str) -> &'static str {
    let token = compact_alnum_ascii(raw, 64);
    if token.is_empty() {
        return UNKNOWN_SLUG;
    }
    for (alias, canonical) in MANUFACTURER_ALIASES {
        if token == *alias {
            return canonical;
        }
    }
    for (alias, canonical) in MANUFACTURER_ALIASES {
        if alias.len() >= 4 && token.starts_with(alias) {
            return canonical;
        }
    }
    UNKNOWN_SLUG
}

pub fn normalize_model_slug(manufacturer: &str, raw_model: &str) -> &'static str {
    let token = compact_alnum_ascii(raw_model, 96);
    if token.is_empty() {
        return UNKNOWN_SLUG;
    }
    let stripped_brand = manufacturer.replace('_', "");
    let trimmed = token.strip_prefix(&stripped_brand).unwrap_or(&token);
    for (brand, alias, canonical) in MODEL_ALIASES {
        let brand_compact = brand.replace('_', "");
        let brand_matches = manufacturer == *brand
            || (manufacturer == "tp_link" && *brand == "tapo")
            || (manufacturer == UNKNOWN_SLUG && token.starts_with(&brand_compact));
        if !brand_matches {
            continue;
        }
        let brand_trimmed = token.strip_prefix(&brand_compact).unwrap_or(trimmed);
        if model_token_matches(trimmed, alias)
            || model_token_matches(brand_trimmed, alias)
            || token == *alias
        {
            return canonical;
        }
    }
    UNKNOWN_SLUG
}

fn model_token_matches(candidate: &str, alias: &str) -> bool {
    if candidate == alias {
        return true;
    }
    if alias.len() < 4 {
        return false;
    }
    let Some(rest) = candidate.strip_prefix(alias) else {
        return false;
    };
    // Avoid matching a longer model number when both alias and suffix are numeric.
    let alias_ends_digit = alias.bytes().last().is_some_and(|b| b.is_ascii_digit());
    let rest_starts_digit = rest.bytes().next().is_some_and(|b| b.is_ascii_digit());
    !(alias_ends_digit && rest_starts_digit)
}

pub fn normalize_release_channel(version: &str) -> ReleaseChannel {
    let Some((_, pre)) = version.split_once('-') else {
        return ReleaseChannel::Stable;
    };
    let lower = pre.to_ascii_lowercase();
    if lower.starts_with("alpha") {
        ReleaseChannel::Alpha
    } else if lower.starts_with("beta") || lower.starts_with("rc") {
        ReleaseChannel::Beta
    } else if lower.starts_with("nightly") {
        ReleaseChannel::Nightly
    } else if lower.starts_with("dev") {
        ReleaseChannel::Dev
    } else {
        ReleaseChannel::Unknown
    }
}

pub const fn normalize_deployment_type(is_container: bool) -> DeploymentType {
    if is_container {
        DeploymentType::Container
    } else {
        DeploymentType::Native
    }
}

pub fn normalize_os_family(raw_os: &str) -> OsFamily {
    let lower = raw_os.trim().to_ascii_lowercase();
    if lower.contains("linux")
        || lower.contains("ubuntu")
        || lower.contains("debian")
        || lower.contains("fedora")
        || lower.contains("alpine")
        || lower.contains("arch")
        || lower.contains("nixos")
    {
        OsFamily::Linux
    } else if lower.contains("mac") || lower.contains("darwin") {
        OsFamily::Macos
    } else if lower.contains("win") {
        OsFamily::Windows
    } else if lower.contains("bsd") {
        OsFamily::Bsd
    } else if lower.is_empty() {
        OsFamily::Unknown
    } else {
        OsFamily::Other
    }
}

/// Extracts at most `major` or `major.minor` digits, or `"rolling"` / `"unknown"`.
pub fn normalize_os_version(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return UNKNOWN_SLUG.to_owned();
    };
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("rolling") {
        return "rolling".to_owned();
    }
    let mut major = String::new();
    let mut minor = String::new();
    let mut seen_digit = false;
    let mut in_minor = false;
    for ch in trimmed.chars() {
        if ch.is_ascii_digit() {
            seen_digit = true;
            if in_minor {
                if minor.len() < 4 {
                    minor.push(ch);
                } else {
                    break;
                }
            } else if major.len() < 4 {
                major.push(ch);
            } else {
                break;
            }
        } else if ch == '.' && seen_digit && !in_minor {
            in_minor = true;
        } else if seen_digit {
            break;
        }
    }
    if major.is_empty() {
        UNKNOWN_SLUG.to_owned()
    } else if minor.is_empty() {
        major
    } else {
        format!("{major}.{minor}")
    }
}

pub fn normalize_architecture(raw: &str) -> Architecture {
    match raw.trim().to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" | "x64" => Architecture::X86_64,
        "aarch64" | "arm64" => Architecture::Aarch64,
        "arm" | "armv7" | "armv7l" | "armhf" => Architecture::Arm,
        "riscv64" => Architecture::Riscv64,
        "" => Architecture::Unknown,
        _ => Architecture::Other,
    }
}

/// Normalizes a raw CPU brand description into a closed vendor and model family pair.
pub fn normalize_cpu(raw_brand: Option<&str>) -> (CpuVendor, CpuModelFamily) {
    let Some(raw) = raw_brand else {
        return (CpuVendor::Unknown, CpuModelFamily::Unknown);
    };
    let lower = raw.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return (CpuVendor::Unknown, CpuModelFamily::Unknown);
    }
    if lower.contains("intel") {
        return (CpuVendor::Intel, normalize_intel_family(&lower));
    }
    if lower.contains("amd") || lower.contains("ryzen") || lower.contains("epyc") {
        return (CpuVendor::Amd, normalize_amd_family(&lower));
    }
    if lower.contains("apple") {
        return (CpuVendor::Apple, normalize_apple_family(&lower));
    }
    if lower.contains("qualcomm") || lower.contains("snapdragon") {
        return (CpuVendor::Qualcomm, CpuModelFamily::QualcommSnapdragon);
    }
    if lower.contains("ampere") || lower.contains("altra") {
        return (CpuVendor::Ampere, CpuModelFamily::AmpereAltra);
    }
    if lower.contains("neoverse") {
        return (CpuVendor::Arm, CpuModelFamily::ArmNeoverse);
    }
    if lower.contains("cortex-a") || lower.contains("cortex a") || lower.contains("raspberry pi") {
        return (CpuVendor::Arm, CpuModelFamily::ArmCortexA);
    }
    (CpuVendor::Other, CpuModelFamily::Unknown)
}

fn normalize_intel_family(lower: &str) -> CpuModelFamily {
    if lower.contains("core(tm) ultra") || lower.contains("core ultra") {
        CpuModelFamily::IntelCoreUltra
    } else if lower.contains("core(tm) i") || lower.contains("core i") {
        CpuModelFamily::IntelCoreI
    } else if lower.contains("xeon") {
        CpuModelFamily::IntelXeon
    } else if ["n100", "n95", "n97", "n150", "n200", "n305", "processor n"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        CpuModelFamily::IntelNSeries
    } else if lower.contains("celeron") || lower.contains("pentium") || lower.contains("atom") {
        CpuModelFamily::IntelAtomCeleronPentium
    } else {
        CpuModelFamily::Unknown
    }
}

fn normalize_amd_family(lower: &str) -> CpuModelFamily {
    if lower.contains("threadripper") {
        CpuModelFamily::AmdThreadripper
    } else if lower.contains("ryzen") {
        CpuModelFamily::AmdRyzen
    } else if lower.contains("epyc") {
        CpuModelFamily::AmdEpyc
    } else if lower.contains("athlon") {
        CpuModelFamily::AmdAthlon
    } else {
        CpuModelFamily::Unknown
    }
}

fn normalize_apple_family(lower: &str) -> CpuModelFamily {
    if lower.contains("m1") {
        CpuModelFamily::AppleM1
    } else if lower.contains("m2") {
        CpuModelFamily::AppleM2
    } else if lower.contains("m3") {
        CpuModelFamily::AppleM3
    } else if lower.contains("m4") {
        CpuModelFamily::AppleM4
    } else if lower.contains("silicon") || lower.contains("apple m") {
        CpuModelFamily::AppleMOther
    } else {
        CpuModelFamily::Unknown
    }
}

pub const fn normalize_memory_bucket(total_bytes: u64) -> MemoryBucket {
    const GIB: u64 = 1024 * 1024 * 1024;
    if total_bytes == 0 {
        MemoryBucket::Unknown
    } else if total_bytes < 2 * GIB {
        MemoryBucket::Lt2Gb
    } else if total_bytes < 4 * GIB {
        MemoryBucket::From2To4Gb
    } else if total_bytes < 8 * GIB {
        MemoryBucket::From4To8Gb
    } else if total_bytes < 16 * GIB {
        MemoryBucket::From8To16Gb
    } else if total_bytes < 32 * GIB {
        MemoryBucket::From16To32Gb
    } else if total_bytes < 64 * GIB {
        MemoryBucket::From32To64Gb
    } else if total_bytes < 128 * GIB {
        MemoryBucket::From64To128Gb
    } else {
        MemoryBucket::Gte128Gb
    }
}

pub fn normalize_video_acceleration(raw: &str) -> VideoAcceleration {
    match raw.trim().to_ascii_lowercase().as_str() {
        "vaapi" | "va-api" => VideoAcceleration::Vaapi,
        "nvenc" | "nvdec" | "cuda" => VideoAcceleration::Nvenc,
        "qsv" | "quicksync" => VideoAcceleration::Qsv,
        "videotoolbox" | "vt" => VideoAcceleration::Videotoolbox,
        "v4l2" | "v4l2m2m" => VideoAcceleration::V4l2,
        "d3d11va" | "dxva2" => VideoAcceleration::D3d11va,
        "vulkan" => VideoAcceleration::Vulkan,
        "none" | "software" | "cpu" => VideoAcceleration::None,
        _ => VideoAcceleration::Unknown,
    }
}

pub fn normalize_camera_backend(raw: &str) -> TelemetryCameraBackend {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => TelemetryCameraBackend::Auto,
        "retina" | "rtsp" => TelemetryCameraBackend::Retina,
        "reo-proto" | "reo_proto" | "reoproto" | "reolink" => TelemetryCameraBackend::ReoProto,
        _ => TelemetryCameraBackend::Unknown,
    }
}

pub fn normalize_camera_transport(raw: &str) -> TelemetryCameraTransport {
    match raw.trim().to_ascii_lowercase().as_str() {
        "tcp" => TelemetryCameraTransport::Tcp,
        "udp" => TelemetryCameraTransport::Udp,
        _ => TelemetryCameraTransport::Unknown,
    }
}

pub fn normalize_video_codec(raw: Option<&str>) -> VideoCodec {
    let Some(raw) = raw else {
        return VideoCodec::Unknown;
    };
    let lower = raw.trim().to_ascii_lowercase();
    if lower.contains("264") || lower.contains("avc") {
        VideoCodec::H264
    } else if lower.contains("265") || lower.contains("hevc") || lower.contains("hvc1") {
        VideoCodec::H265
    } else if lower.contains("mjpeg") || lower.contains("jpeg") {
        VideoCodec::Mjpeg
    } else if lower.contains("vp8") {
        VideoCodec::Vp8
    } else if lower.contains("vp9") {
        VideoCodec::Vp9
    } else if lower.contains("av1") || lower.contains("av01") {
        VideoCodec::Av1
    } else {
        VideoCodec::Unknown
    }
}

pub fn normalize_resolution_bucket(raw: Option<&str>) -> ResolutionBucket {
    let Some(raw) = raw else {
        return ResolutionBucket::Unknown;
    };
    let trimmed = raw.trim().to_ascii_lowercase();
    let Some((w_str, h_str)) = trimmed.split_once('x') else {
        return ResolutionBucket::Unknown;
    };
    let (Ok(width), Ok(height)) = (w_str.trim().parse::<u64>(), h_str.trim().parse::<u64>()) else {
        return ResolutionBucket::Unknown;
    };
    if width == 0 || height == 0 {
        return ResolutionBucket::Unknown;
    }
    let pixels = width.saturating_mul(height);
    if pixels < 921_600 {
        ResolutionBucket::Sd
    } else if pixels < 2_073_600 {
        ResolutionBucket::Hd720p
    } else if pixels < 3_686_400 {
        ResolutionBucket::Fhd1080p
    } else if pixels < 8_294_400 {
        ResolutionBucket::Qhd1440p
    } else if pixels <= 8_847_360 {
        ResolutionBucket::Uhd4k
    } else {
        ResolutionBucket::Above4k
    }
}

pub fn normalize_recording_mode(raw: &str) -> TelemetryRecordingMode {
    match raw.trim().to_ascii_lowercase().as_str() {
        "continuous" | "always" => TelemetryRecordingMode::Continuous,
        "event" | "event_only" | "event-only" | "events" => TelemetryRecordingMode::EventOnly,
        "disabled" | "off" | "none" => TelemetryRecordingMode::Disabled,
        _ => TelemetryRecordingMode::Unknown,
    }
}

pub const fn normalize_storage_capacity_bucket(bytes: u64) -> StorageCapacityBucket {
    const GIB: u64 = 1024 * 1024 * 1024;
    const TIB: u64 = 1024 * GIB;
    if bytes == 0 {
        StorageCapacityBucket::Unknown
    } else if bytes < 250 * GIB {
        StorageCapacityBucket::Lt250Gb
    } else if bytes < TIB {
        StorageCapacityBucket::From250GbTo1Tb
    } else if bytes < 4 * TIB {
        StorageCapacityBucket::From1TbTo4Tb
    } else if bytes < 16 * TIB {
        StorageCapacityBucket::From4TbTo16Tb
    } else if bytes < 64 * TIB {
        StorageCapacityBucket::From16TbTo64Tb
    } else {
        StorageCapacityBucket::Gte64Tb
    }
}

pub fn normalize_projected_retention_bucket(days: Option<f64>) -> ProjectedRetentionBucket {
    let Some(days) = days else {
        return ProjectedRetentionBucket::Unknown;
    };
    if !days.is_finite() || days <= 0.0 {
        ProjectedRetentionBucket::Unknown
    } else if days < 1.0 {
        ProjectedRetentionBucket::Lt1Day
    } else if days < 7.0 {
        ProjectedRetentionBucket::From1To7Days
    } else if days < 30.0 {
        ProjectedRetentionBucket::From7To30Days
    } else if days < 90.0 {
        ProjectedRetentionBucket::From30To90Days
    } else if days < 365.0 {
        ProjectedRetentionBucket::From90To365Days
    } else {
        ProjectedRetentionBucket::Gte365Days
    }
}

fn compact_alnum_ascii(raw: &str, max_len: usize) -> String {
    let mut output = String::with_capacity(raw.len().min(max_len));
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            if output.len() == max_len {
                break;
            }
        }
    }
    output
}
