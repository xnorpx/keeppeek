use super::{RegistryError, deserialize_whole_u32};
use serde::{Deserialize, Serialize};

const DISPLAY_VERSION: u32 = 1;
const MAX_STREAMS: u32 = 12;
const MAX_DECORATION_PX: u32 = 24;
const DEFAULT_DECORATION_PX: u32 = 10;

pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Option<DisplaySettings>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<serde_json::Map<String, serde_json::Value>>::deserialize(deserializer)?
        .map(|fields| {
            serde_json::from_value(serde_json::Value::Object(fields))
                .map_err(serde::de::Error::custom)
        })
        .transpose()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DisplaySettings {
    #[serde(deserialize_with = "deserialize_whole_u32")]
    version: u32,
    tile_shape: TileShape,
    media_fit: MediaFit,
    streaming_mode: StreamingMode,
    #[serde(deserialize_with = "deserialize_whole_u32")]
    stream_limit: u32,
    keep_awake: bool,
    #[serde(deserialize_with = "deserialize_whole_u32")]
    gap_px: u32,
    #[serde(deserialize_with = "deserialize_whole_u32")]
    corner_radius_px: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String")]
enum TileShape {
    #[serde(rename = "16:9")]
    Widescreen,
    #[serde(rename = "4:3")]
    Standard,
    #[serde(rename = "native")]
    Native,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase", try_from = "String")]
enum MediaFit {
    Contain,
    Cover,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase", try_from = "String")]
enum StreamingMode {
    Smart,
    Continuous,
}

impl TryFrom<String> for TileShape {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "16:9" => Ok(Self::Widescreen),
            "4:3" => Ok(Self::Standard),
            "native" => Ok(Self::Native),
            _ => Err("dashboard tile shape is invalid"),
        }
    }
}

impl TryFrom<String> for MediaFit {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "contain" => Ok(Self::Contain),
            "cover" => Ok(Self::Cover),
            _ => Err("dashboard media fit is invalid"),
        }
    }
}

impl TryFrom<String> for StreamingMode {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "smart" => Ok(Self::Smart),
            "continuous" => Ok(Self::Continuous),
            _ => Err("dashboard streaming mode is invalid"),
        }
    }
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            version: DISPLAY_VERSION,
            tile_shape: TileShape::Widescreen,
            media_fit: MediaFit::Contain,
            streaming_mode: StreamingMode::Smart,
            stream_limit: MAX_STREAMS,
            keep_awake: false,
            gap_px: DEFAULT_DECORATION_PX,
            corner_radius_px: DEFAULT_DECORATION_PX,
        }
    }
}

impl DisplaySettings {
    pub(super) fn validate(&self) -> Result<(), RegistryError> {
        if self.version != DISPLAY_VERSION {
            return Err(RegistryError::new(
                "dashboard display version is unsupported",
            ));
        }
        if !(1..=MAX_STREAMS).contains(&self.stream_limit) {
            return Err(RegistryError::new("dashboard stream limit is invalid"));
        }
        if self.gap_px > MAX_DECORATION_PX || self.corner_radius_px > MAX_DECORATION_PX {
            return Err(RegistryError::new(
                "dashboard spacing or corner radius is invalid",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::DisplaySettings;
    use crate::server::peek_layouts::{LayoutRegistry, RegistryError, RegistryStore};

    #[test]
    fn dashboard_display_rejects_array_shaped_documents() {
        let mut layout =
            serde_json::to_value(crate::server::peek_layouts::default_layout(&[])).unwrap();
        layout["display"] = serde_json::json!([1, "16:9", "contain", "smart", 12, false, 10, 10]);
        assert!(serde_json::from_value::<crate::server::peek_layouts::Layout>(layout).is_err());
    }

    #[test]
    fn display_validation_rejects_invalid_fields_before_persistence() {
        let defaults = serde_json::to_value(DisplaySettings::default()).unwrap();
        for (field, value) in [
            ("version", serde_json::json!(2)),
            ("stream_limit", serde_json::json!(0)),
            ("stream_limit", serde_json::json!(13)),
            ("stream_limit", serde_json::json!(2.5)),
            ("gap_px", serde_json::json!(-1)),
            ("gap_px", serde_json::json!(25)),
            ("corner_radius_px", serde_json::json!(25)),
            ("corner_radius_px", serde_json::json!("2")),
            ("keep_awake", serde_json::json!("true")),
            ("tile_shape", serde_json::json!("stretch")),
            ("media_fit", serde_json::json!({"cover": null})),
            ("streaming_mode", serde_json::json!("unlimited")),
            ("unknown", serde_json::json!(true)),
        ] {
            let mut document = defaults.clone();
            document[field] = value;
            let accepted = serde_json::from_value::<DisplaySettings>(document)
                .is_ok_and(|settings| settings.validate().is_ok());
            assert!(!accepted, "invalid field {field} must be rejected");
        }
        for boundary in [0, 24] {
            let mut document = defaults.clone();
            document["gap_px"] = boundary.into();
            document["corner_radius_px"] = boundary.into();
            let settings: DisplaySettings = serde_json::from_value(document).unwrap();
            settings.validate().unwrap();
        }
    }

    #[test]
    fn legacy_writes_preserve_display_and_users_cannot_change_it() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-dashboard-display-legacy-{}",
            uuid::Uuid::new_v4()
        ));
        let path = directory.join("config.toml");
        let camera_ids = vec!["front".to_owned()];
        let mut store = RegistryStore::open(path.clone(), &camera_ids).unwrap();
        let mut candidate = store.registry_for_principal("admin", true);
        let display = DisplaySettings {
            gap_px: 0,
            corner_radius_px: 0,
            ..DisplaySettings::default()
        };
        candidate.layouts[0].display = Some(display.clone());
        store
            .replace_for("admin", true, store.revision(), candidate)
            .unwrap();
        let expected = std::fs::read(&path).unwrap();
        let mut user = store.registry_for("viewer");
        user.layouts[0].display = Some(DisplaySettings::default());
        assert!(matches!(
            store.replace_for("viewer", false, store.revision(), user),
            Err(RegistryError::NotAuthorized { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), expected);

        let mut legacy = serde_json::to_value(store.registry_for_principal("admin", true)).unwrap();
        legacy["layouts"][0]
            .as_object_mut()
            .unwrap()
            .remove("display");
        let candidate: LayoutRegistry = serde_json::from_value(legacy).unwrap();
        store
            .replace_for("admin", true, store.revision(), candidate)
            .unwrap();
        let reopened = RegistryStore::open(path.clone(), &camera_ids).unwrap();
        assert_eq!(
            reopened.registry_for("viewer").layouts[0].display,
            Some(display)
        );
        let before_conflict = std::fs::read(&path).unwrap();
        let candidate = store.registry_for_principal("admin", true);
        assert!(matches!(
            store.replace_for("admin", true, 1, candidate),
            Err(RegistryError::Conflict { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before_conflict);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
