//! Loads the embedded CCTV camera catalog into immutable lookup indexes.

use crate::camera_catalog::{OnvifPortFrequency, OnvifPortReport};
use crate::test_support::TestCatalogCamera;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{Cursor, Read},
    net::IpAddr,
};
use url::Url;
use zip::ZipArchive;

const EMBEDDED_ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cameras.zip"));
const JSON_FILE: &str = "cameras.json";
const CSV_FILE: &str = "cameras.csv";
const METADATA_FILE: &str = "release-metadata.json";
const REQUIRED_FILES: &[&str] = &[JSON_FILE, CSV_FILE, METADATA_FILE];
// The published catalog is currently below 10 MiB per member. This cap prevents an inflated
// upstream archive from allocating an unbounded buffer during startup.
const MAX_MEMBER_BYTES: u64 = 16 * 1024 * 1024;
// The release archive currently has three members. Allowing a small margin keeps future metadata
// additions possible while rejecting archive layouts that do not match the expected release shape.
const MAX_ARCHIVE_MEMBERS: usize = 16;
const MAX_SEARCH_RESULTS: usize = 50;

/// Immutable metadata for the embedded camera catalog release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogMetadata {
    pub(crate) version: String,
    pub(crate) tag: String,
    pub(crate) generated_at: String,
    pub(crate) camera_count: usize,
}

/// A catalog camera available for discovery enrichment or manual selection.
#[derive(Debug, Clone)]
pub struct CatalogCamera {
    pub(crate) id: String,
    pub(crate) brand: String,
    pub(crate) model: String,
    pub(crate) aliases: Box<[String]>,
    pub(crate) camera_type: String,
    pub(crate) resolution_label: Option<String>,
    pub(crate) megapixels: Option<f64>,
    pub(crate) sensor: Option<String>,
    pub(crate) field_of_view: Option<String>,
    pub(crate) night_vision: Option<String>,
    pub(crate) ip_rating: Option<String>,
    pub(crate) ik_rating: Option<String>,
    pub(crate) two_way_audio: Option<bool>,
    pub(crate) release_year: Option<u16>,
    pub(crate) community_notes_count: u32,
    pub(crate) protocols: Box<[String]>,
    pub(crate) codecs: Box<[String]>,
    pub(crate) streams: Box<[CatalogStream]>,
    pub(crate) sources: Box<[String]>,
    onvif_port: Option<u16>,
    stream_templates: StreamTemplates,
    search_terms: Box<[String]>,
}

/// A stream capability declared by the camera catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogStream {
    pub(crate) name: String,
    pub(crate) resolution: Option<String>,
    pub(crate) fps: Option<u16>,
    pub(crate) codec: Option<String>,
}

/// Credential-free RTSP endpoints rendered from catalog templates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamHints {
    pub(crate) main: Option<String>,
    pub(crate) sub: Option<String>,
}

/// The outcome of matching discovered camera metadata to the catalog.
#[derive(Debug, Clone)]
pub enum CameraMatch {
    Exact(Box<CatalogCamera>),
    Ambiguous,
    Missing,
}

/// In-memory indexes for the embedded camera catalog.
#[derive(Debug)]
pub struct CameraDatabase {
    metadata: CatalogMetadata,
    cameras: Box<[CatalogCamera]>,
    by_id: HashMap<String, usize>,
    by_brand_model: HashMap<String, Box<[usize]>>,
    by_model: HashMap<String, Box<[usize]>>,
}

impl CameraDatabase {
    /// Loads the camera catalog embedded in the KeepPeek executable.
    pub(crate) fn load_embedded() -> anyhow::Result<Self> {
        Self::from_archive(EMBEDDED_ARCHIVE)
    }

    pub(crate) fn from_test_cameras(
        cameras: impl IntoIterator<Item = TestCatalogCamera>,
    ) -> anyhow::Result<Self> {
        let mut catalog_cameras = Vec::new();
        for camera in cameras {
            catalog_cameras.push(CatalogCamera::from_test_camera(camera)?);
        }
        let cameras = catalog_cameras.into_boxed_slice();
        let (by_id, by_brand_model, by_model) = build_indexes(&cameras)?;
        Ok(Self {
            metadata: CatalogMetadata {
                version: "test".to_owned(),
                tag: "test".to_owned(),
                generated_at: "1970-01-01T00:00:00Z".to_owned(),
                camera_count: cameras.len(),
            },
            cameras,
            by_id,
            by_brand_model,
            by_model,
        })
    }

    /// Returns immutable metadata supplied by the upstream catalog release.
    pub(crate) const fn metadata(&self) -> &CatalogMetadata {
        &self.metadata
    }

    /// Matches a discovered manufacturer and model to one catalog camera when unambiguous.
    pub(crate) fn match_camera(&self, brand: &str, model: &str) -> CameraMatch {
        let brand_model = brand_model_key(brand, model);
        if let Some(indexes) = self.by_brand_model.get(&brand_model) {
            return self.match_indexes(indexes);
        }

        let model = normalize(model);
        if model.is_empty() {
            return CameraMatch::Missing;
        }
        self.by_model
            .get(&model)
            .map_or(CameraMatch::Missing, |indexes| self.match_indexes(indexes))
    }

    /// Searches catalog camera names and aliases with a bounded result set.
    pub(crate) fn search(&self, query: &str, limit: usize) -> Vec<CatalogCamera> {
        let query = normalize(query);
        if query.is_empty() {
            return Vec::new();
        }

        let limit = limit.min(MAX_SEARCH_RESULTS);
        let mut matches = self
            .cameras
            .iter()
            .filter(|camera| camera.search_terms.iter().any(|term| term.contains(&query)))
            .collect::<Vec<_>>();
        matches.sort_unstable_by(|left, right| {
            search_rank(left, &query)
                .cmp(&search_rank(right, &query))
                .then_with(|| left.brand.cmp(&right.brand))
                .then_with(|| left.model.cmp(&right.model))
        });
        matches.into_iter().take(limit).cloned().collect()
    }

    /// Renders optional catalog stream templates for a specific camera address.
    pub(crate) fn stream_hints(&self, id: &str, ip: IpAddr) -> Option<StreamHints> {
        let camera = self.by_id.get(id).map(|index| &self.cameras[*index])?;
        let main = camera
            .stream_templates
            .main
            .as_deref()
            .and_then(|template| render_stream_template(template, ip));
        let sub = camera
            .stream_templates
            .sub
            .as_deref()
            .and_then(|template| render_stream_template(template, ip));
        (main.is_some() || sub.is_some()).then_some(StreamHints { main, sub })
    }

    pub(crate) fn onvif_port_report(&self) -> OnvifPortReport {
        let mut frequencies = BTreeMap::<u16, usize>::new();
        let mut onvif_capable_camera_count = 0;
        for camera in &self.cameras {
            if !camera
                .protocols
                .iter()
                .any(|protocol| protocol.eq_ignore_ascii_case("onvif"))
            {
                continue;
            }
            onvif_capable_camera_count += 1;
            if let Some(port) = camera.onvif_port {
                *frequencies.entry(port).or_default() += 1;
            }
        }
        let mut catalog_port_frequencies = frequencies
            .into_iter()
            .map(|(port, camera_count)| OnvifPortFrequency::new(port, camera_count))
            .collect::<Vec<_>>();
        catalog_port_frequencies.sort_unstable_by(|left, right| {
            right
                .camera_count()
                .cmp(&left.camera_count())
                .then_with(|| left.port().cmp(&right.port()))
        });
        OnvifPortReport::new(
            self.cameras.len(),
            onvif_capable_camera_count,
            catalog_port_frequencies.into_boxed_slice(),
        )
    }

    fn from_archive(bytes: &[u8]) -> anyhow::Result<Self> {
        let mut archive = ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| anyhow::anyhow!("invalid camera database ZIP: {error}"))?;
        validate_archive_entries(&mut archive)?;

        let metadata_bytes = read_member(&mut archive, METADATA_FILE)?;
        let metadata: ReleaseMetadata = serde_json::from_slice(&metadata_bytes)
            .map_err(|error| anyhow::anyhow!("invalid camera database metadata: {error}"))?;
        if metadata.schema_version != 1 {
            anyhow::bail!(
                "unsupported camera database metadata schema {}",
                metadata.schema_version
            );
        }

        let json_bytes = read_member(&mut archive, JSON_FILE)?;
        let csv_bytes = read_member(&mut archive, CSV_FILE)?;
        verify_release_file(&metadata, JSON_FILE, &json_bytes)?;
        verify_release_file(&metadata, CSV_FILE, &csv_bytes)?;

        let records: Vec<JsonCamera> = serde_json::from_slice(&json_bytes)
            .map_err(|error| anyhow::anyhow!("invalid camera database JSON: {error}"))?;
        if records.len() != metadata.camera_count {
            anyhow::bail!(
                "camera database metadata declares {} cameras but JSON contains {}",
                metadata.camera_count,
                records.len()
            );
        }

        let mut csv_records = parse_csv(&csv_bytes)?;
        let mut cameras = Vec::with_capacity(records.len());
        for record in records {
            let summary = csv_records.remove(&record.id).ok_or_else(|| {
                anyhow::anyhow!("camera database CSV is missing camera ID {}", record.id)
            })?;
            validate_summary(&record, &summary)?;
            cameras.push(CatalogCamera::from_records(record, summary));
        }
        if !csv_records.is_empty() {
            anyhow::bail!("camera database CSV has records absent from the JSON catalog");
        }

        let cameras = cameras.into_boxed_slice();
        let (by_id, by_brand_model, by_model) = build_indexes(&cameras)?;
        Ok(Self {
            metadata: CatalogMetadata {
                version: metadata.version,
                tag: metadata.tag,
                generated_at: metadata.generated_at,
                camera_count: metadata.camera_count,
            },
            cameras,
            by_id,
            by_brand_model,
            by_model,
        })
    }

    fn match_indexes(&self, indexes: &[usize]) -> CameraMatch {
        match indexes {
            [index] => CameraMatch::Exact(Box::new(self.cameras[*index].clone())),
            [] => CameraMatch::Missing,
            _ => CameraMatch::Ambiguous,
        }
    }
}

impl CatalogCamera {
    fn from_test_camera(camera: TestCatalogCamera) -> anyhow::Result<Self> {
        let id = camera.id.trim().to_owned();
        let brand = camera.brand.trim().to_owned();
        let model = camera.model.trim().to_owned();
        if id.is_empty() || brand.is_empty() || model.is_empty() {
            anyhow::bail!("test catalog camera ID, brand, and model must be nonempty");
        }

        let aliases = camera.aliases.into_boxed_slice();
        let search_terms = camera_search_terms(&brand, &model, &aliases);

        Ok(Self {
            id,
            brand,
            model,
            aliases,
            camera_type: "test".to_owned(),
            resolution_label: Some("Test".to_owned()),
            megapixels: None,
            sensor: None,
            field_of_view: None,
            night_vision: None,
            ip_rating: None,
            ik_rating: None,
            two_way_audio: None,
            release_year: None,
            community_notes_count: 0,
            protocols: vec!["onvif".to_owned(), "rtsp".to_owned()].into_boxed_slice(),
            codecs: vec!["H.264".to_owned()].into_boxed_slice(),
            streams: vec![
                CatalogStream {
                    name: "main".to_owned(),
                    resolution: None,
                    fps: None,
                    codec: Some("H.264".to_owned()),
                },
                CatalogStream {
                    name: "sub".to_owned(),
                    resolution: None,
                    fps: None,
                    codec: Some("H.264".to_owned()),
                },
            ]
            .into_boxed_slice(),
            sources: vec!["https://keeppeek.invalid/test-camera".to_owned()].into_boxed_slice(),
            onvif_port: None,
            stream_templates: StreamTemplates {
                main: camera.main_rtsp_template,
                sub: camera.sub_rtsp_template,
            },
            search_terms,
        })
    }

    fn from_records(record: JsonCamera, summary: CsvCamera) -> Self {
        let search_terms = camera_search_terms(&record.brand, &record.model, &record.aliases);
        let onvif_port = record.onvif_port.or(summary.onvif_port);

        let video = record.video.unwrap_or_default();
        Self {
            onvif_port,
            id: record.id,
            brand: record.brand,
            model: record.model,
            aliases: record.aliases.into_boxed_slice(),
            camera_type: summary.camera_type,
            resolution_label: summary.resolution_label.or(record.resolution.label),
            megapixels: summary.megapixels.or(Some(record.resolution.megapixels)),
            sensor: summary.sensor,
            field_of_view: summary.field_of_view_deg,
            night_vision: summary.night_vision_type,
            ip_rating: summary.ip_rating,
            ik_rating: summary.ik_rating,
            two_way_audio: summary.two_way_audio,
            release_year: summary.release_year,
            community_notes_count: summary.community_notes_count,
            protocols: record.protocols.into_boxed_slice(),
            codecs: video.codecs.into_boxed_slice(),
            streams: video
                .streams
                .into_iter()
                .filter_map(CatalogStream::from_json)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            sources: record.sources.into_boxed_slice(),
            stream_templates: record
                .configs
                .and_then(|configs| configs.frigate)
                .map_or_else(StreamTemplates::default, StreamTemplates::from_frigate),
            search_terms,
        }
    }
}

impl CatalogStream {
    fn from_json(stream: JsonStream) -> Option<Self> {
        let name = stream.name?.trim().to_owned();
        (!name.is_empty()).then_some(Self {
            name,
            resolution: nonempty(stream.resolution),
            fps: stream.fps,
            codec: nonempty(stream.codec),
        })
    }
}

#[derive(Debug, Clone, Default)]
struct StreamTemplates {
    main: Option<String>,
    sub: Option<String>,
}

impl StreamTemplates {
    fn from_frigate(frigate: FrigateConfig) -> Self {
        Self {
            main: nonempty(frigate.rtsp_url_template),
            sub: nonempty(frigate.best_substream),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReleaseMetadata {
    schema_version: u32,
    version: String,
    tag: String,
    generated_at: String,
    camera_count: usize,
    files: Vec<ReleaseFile>,
}

#[derive(Debug, Deserialize)]
struct ReleaseFile {
    name: String,
    bytes: usize,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct JsonCamera {
    id: String,
    brand: String,
    model: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(rename = "type")]
    camera_type: String,
    resolution: JsonResolution,
    #[serde(default)]
    protocols: Vec<String>,
    #[serde(default)]
    onvif_port: Option<u16>,
    #[serde(default)]
    video: Option<JsonVideo>,
    #[serde(default)]
    configs: Option<JsonConfigs>,
    #[serde(default)]
    sources: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct JsonResolution {
    megapixels: f64,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct JsonVideo {
    #[serde(default)]
    codecs: Vec<String>,
    #[serde(default)]
    streams: Vec<JsonStream>,
}

#[derive(Debug, Deserialize)]
struct JsonStream {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    fps: Option<u16>,
    #[serde(default)]
    codec: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JsonConfigs {
    #[serde(default)]
    frigate: Option<FrigateConfig>,
}

#[derive(Debug, Deserialize)]
struct FrigateConfig {
    #[serde(default)]
    rtsp_url_template: Option<String>,
    #[serde(default)]
    best_substream: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CsvCamera {
    id: String,
    brand: String,
    model: String,
    #[serde(rename = "type")]
    camera_type: String,
    #[serde(default)]
    resolution_label: Option<String>,
    #[serde(default)]
    megapixels: Option<f64>,
    #[serde(default)]
    sensor: Option<String>,
    #[serde(default)]
    field_of_view_deg: Option<String>,
    #[serde(default)]
    night_vision_type: Option<String>,
    #[serde(default)]
    ip_rating: Option<String>,
    #[serde(default)]
    ik_rating: Option<String>,
    #[serde(default)]
    two_way_audio: Option<bool>,
    #[serde(default)]
    onvif_port: Option<u16>,
    #[serde(default)]
    release_year: Option<u16>,
    community_notes_count: u32,
}

impl CsvCamera {
    fn into_summary(mut self) -> Self {
        self.resolution_label = nonempty(self.resolution_label);
        self.sensor = nonempty(self.sensor);
        self.field_of_view_deg = nonempty(self.field_of_view_deg);
        self.night_vision_type = nonempty(self.night_vision_type);
        self.ip_rating = nonempty(self.ip_rating);
        self.ik_rating = nonempty(self.ik_rating);
        self
    }
}

fn parse_csv(bytes: &[u8]) -> anyhow::Result<HashMap<String, CsvCamera>> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(bytes);
    let mut records = HashMap::new();
    for row in reader.deserialize::<CsvCamera>() {
        let record =
            row.map_err(|error| anyhow::anyhow!("invalid camera database CSV: {error}"))?;
        if record.id.trim().is_empty()
            || record.brand.trim().is_empty()
            || record.model.trim().is_empty()
        {
            anyhow::bail!("camera database CSV has a record without an ID, brand, or model");
        }
        let id = record.id.clone();
        if records.insert(id.clone(), record.into_summary()).is_some() {
            anyhow::bail!("camera database CSV contains duplicate camera ID {id}");
        }
    }
    Ok(records)
}

fn validate_archive_entries(archive: &mut ZipArchive<Cursor<&[u8]>>) -> anyhow::Result<()> {
    if archive.len() > MAX_ARCHIVE_MEMBERS {
        anyhow::bail!(
            "camera database archive has {} members, exceeding the limit of {MAX_ARCHIVE_MEMBERS}",
            archive.len()
        );
    }

    let mut names = HashSet::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| anyhow::anyhow!("invalid camera database ZIP entry: {error}"))?;
        let name = entry.name().to_owned();
        if name.starts_with('/')
            || name.contains('\\')
            || name
                .split('/')
                .any(|component| matches!(component, "." | ".."))
        {
            anyhow::bail!("camera database archive contains unsafe entry {name:?}");
        }
        if !names.insert(name.clone()) {
            anyhow::bail!("camera database archive contains duplicate entry {name:?}");
        }
    }

    for required in REQUIRED_FILES {
        if !names.contains(*required) {
            anyhow::bail!("camera database archive is missing {required}");
        }
    }
    Ok(())
}

fn read_member(archive: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> anyhow::Result<Vec<u8>> {
    let mut entry = archive
        .by_name(name)
        .map_err(|error| anyhow::anyhow!("camera database archive is missing {name}: {error}"))?;
    if entry.is_dir() || entry.size() > MAX_MEMBER_BYTES {
        anyhow::bail!("camera database archive member {name} has an invalid size");
    }
    let capacity = usize::try_from(entry.size())?;
    let mut bytes = Vec::with_capacity(capacity);
    entry.read_to_end(&mut bytes).map_err(|error| {
        anyhow::anyhow!("failed to read camera database member {name}: {error}")
    })?;
    if bytes.len() != capacity {
        anyhow::bail!("camera database member {name} ended before its declared size");
    }
    Ok(bytes)
}

fn verify_release_file(
    metadata: &ReleaseMetadata,
    name: &str,
    contents: &[u8],
) -> anyhow::Result<()> {
    let files = metadata
        .files
        .iter()
        .filter(|file| file.name == name)
        .collect::<Vec<_>>();
    let [file] = files.as_slice() else {
        anyhow::bail!("camera database metadata must contain exactly one {name} record");
    };
    if file.bytes != contents.len() {
        anyhow::bail!(
            "camera database metadata declares {} bytes for {name}, found {}",
            file.bytes,
            contents.len()
        );
    }
    let expected = file.sha256.strip_prefix("sha256:").unwrap_or(&file.sha256);
    let actual = encode_lower_hex(Sha256::digest(contents));
    if !expected.eq_ignore_ascii_case(&actual) {
        anyhow::bail!("camera database checksum does not match metadata for {name}");
    }
    Ok(())
}

fn encode_lower_hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn validate_summary(record: &JsonCamera, summary: &CsvCamera) -> anyhow::Result<()> {
    if normalize(&record.brand) != normalize(&summary.brand)
        || normalize(&record.model) != normalize(&summary.model)
        || normalize(&record.camera_type) != normalize(&summary.camera_type)
    {
        anyhow::bail!(
            "camera database JSON and CSV disagree about camera ID {}",
            record.id
        );
    }
    if record.onvif_port == Some(0) || summary.onvif_port == Some(0) {
        anyhow::bail!("camera database ONVIF port must be between 1 and 65535");
    }
    if let (Some(json_port), Some(csv_port)) = (record.onvif_port, summary.onvif_port)
        && json_port != csv_port
    {
        anyhow::bail!(
            "camera database JSON and CSV disagree about ONVIF port for camera ID {}",
            record.id
        );
    }
    Ok(())
}

type CameraIndexes = (
    HashMap<String, usize>,
    HashMap<String, Box<[usize]>>,
    HashMap<String, Box<[usize]>>,
);

fn build_indexes(cameras: &[CatalogCamera]) -> anyhow::Result<CameraIndexes> {
    let mut by_id = HashMap::with_capacity(cameras.len());
    let mut by_brand_model = HashMap::<String, Vec<usize>>::with_capacity(cameras.len());
    let mut by_model = HashMap::<String, Vec<usize>>::with_capacity(cameras.len());

    for (index, camera) in cameras.iter().enumerate() {
        if by_id.insert(camera.id.clone(), index).is_some() {
            anyhow::bail!(
                "camera database JSON contains duplicate camera ID {}",
                camera.id
            );
        }
        for model in std::iter::once(&camera.model).chain(camera.aliases.iter()) {
            let model_key = normalize(model);
            if model_key.is_empty() {
                continue;
            }
            insert_index(
                &mut by_brand_model,
                brand_model_key(&camera.brand, model),
                index,
            );
            insert_index(&mut by_model, model_key, index);
        }
    }

    Ok((
        by_id,
        into_compact_index(by_brand_model),
        into_compact_index(by_model),
    ))
}

fn insert_index(index: &mut HashMap<String, Vec<usize>>, key: String, camera: usize) {
    let candidates = index.entry(key).or_default();
    if !candidates.contains(&camera) {
        candidates.push(camera);
    }
}

fn into_compact_index(index: HashMap<String, Vec<usize>>) -> HashMap<String, Box<[usize]>> {
    index
        .into_iter()
        .map(|(key, candidates)| (key, candidates.into_boxed_slice()))
        .collect()
}

fn search_rank(camera: &CatalogCamera, query: &str) -> u8 {
    let model = normalize(&camera.model);
    if model == query {
        0
    } else if model.starts_with(query) {
        1
    } else if camera.aliases.iter().any(|alias| normalize(alias) == query) {
        2
    } else {
        3
    }
}

fn render_stream_template(template: &str, ip: IpAddr) -> Option<String> {
    if !template.contains("{ip}") {
        return None;
    }
    let endpoint = template
        .replace("{user}", "")
        .replace("{pass}", "")
        .replace("{ip}", &ip.to_string());
    if endpoint.contains('{') || endpoint.contains('}') {
        return None;
    }
    let mut url = Url::parse(&endpoint).ok()?;
    if !matches!(url.scheme(), "rtsp" | "rtsps") || url.host_str()?.parse::<IpAddr>().ok()? != ip {
        return None;
    }
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    Some(url.into())
}

fn brand_model_key(brand: &str, model: &str) -> String {
    format!("{}\u{1f}{}", normalize(brand), normalize(model))
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn camera_search_terms(brand: &str, model: &str, aliases: &[String]) -> Box<[String]> {
    let mut terms = Vec::with_capacity(aliases.len() + 3);
    terms.push(normalize(brand));
    terms.push(normalize(model));
    terms.push(normalize(&format!("{brand} {model}")));
    terms.extend(aliases.iter().map(|alias| normalize(alias)));
    terms.retain(|term| !term.is_empty());
    terms.sort_unstable();
    terms.dedup();
    terms.into_boxed_slice()
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|value| (!value.trim().is_empty()).then_some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    #[test]
    fn loads_catalog_from_memory_and_matches_an_alias() {
        let archive = archive_with(
            vec![camera_json(
                "reolink-rlc-811a",
                "Reolink",
                "RLC-811A",
                vec!["RLC 811 A"],
            )],
            "id,brand,model,type,resolution_label,megapixels,sensor,field_of_view_deg,night_vision_type,ip_rating,ik_rating,two_way_audio,release_year,community_notes_count\nreolink-rlc-811a,Reolink,RLC-811A,bullet,4K,8,CMOS,105,hybrid,IP67,IK10,true,2021,0\n",
            None,
        );

        let database = CameraDatabase::from_archive(&archive).unwrap();
        let CameraMatch::Exact(camera) = database.match_camera("Reolink", "RLC 811 A") else {
            panic!("alias should resolve to one camera");
        };

        assert_eq!(database.metadata().version, "2.1.0");
        assert_eq!(camera.id, "reolink-rlc-811a");
        assert_eq!(camera.resolution_label.as_deref(), Some("4K"));
        assert_eq!(camera.two_way_audio, Some(true));
        assert_eq!(camera.streams.len(), 2);
        let onvif_report = database.onvif_port_report();
        assert_eq!(onvif_report.camera_count(), 1);
        assert_eq!(onvif_report.onvif_capable_camera_count(), 1);
        assert!(!onvif_report.has_catalog_port_evidence());
        assert_eq!(
            database
                .search("Reolink RLC-811A", 10)
                .into_iter()
                .map(|camera| camera.id)
                .collect::<Vec<_>>(),
            vec!["reolink-rlc-811a"]
        );
    }

    #[test]
    fn aggregates_onvif_ports_declared_by_matching_json_and_csv_records() {
        let mut record = camera_json("reolink-rlc-811a", "Reolink", "RLC-811A", Vec::new());
        record["onvif_port"] = json!(8000);
        let archive = archive_with(
            vec![record],
            "id,brand,model,type,resolution_label,megapixels,sensor,field_of_view_deg,night_vision_type,ip_rating,ik_rating,two_way_audio,onvif_port,release_year,community_notes_count\nreolink-rlc-811a,Reolink,RLC-811A,bullet,4K,8,CMOS,105,hybrid,IP67,IK10,true,8000,2021,0\n",
            None,
        );

        let report = CameraDatabase::from_archive(&archive)
            .unwrap()
            .onvif_port_report();

        assert_eq!(report.onvif_capable_camera_count(), 1);
        assert_eq!(
            report.catalog_port_frequencies(),
            [OnvifPortFrequency::new(8000, 1)]
        );
    }

    #[test]
    fn rejects_disagreeing_json_and_csv_onvif_ports() {
        let mut record = camera_json("reolink-rlc-811a", "Reolink", "RLC-811A", Vec::new());
        record["onvif_port"] = json!(8000);
        let archive = archive_with(
            vec![record],
            "id,brand,model,type,resolution_label,megapixels,sensor,field_of_view_deg,night_vision_type,ip_rating,ik_rating,two_way_audio,onvif_port,release_year,community_notes_count\nreolink-rlc-811a,Reolink,RLC-811A,bullet,4K,8,CMOS,105,hybrid,IP67,IK10,true,8899,2021,0\n",
            None,
        );

        let error = CameraDatabase::from_archive(&archive).unwrap_err();

        assert!(error.to_string().contains("ONVIF port"));
    }

    #[test]
    fn reports_model_only_matches_as_ambiguous() {
        let archive = archive_with(
            vec![
                camera_json("alpha-cam", "Alpha", "Shared", Vec::new()),
                camera_json("beta-cam", "Beta", "Shared", Vec::new()),
            ],
            "id,brand,model,type,resolution_label,megapixels,sensor,field_of_view_deg,night_vision_type,ip_rating,ik_rating,two_way_audio,release_year,community_notes_count\nalpha-cam,Alpha,Shared,bullet,4K,8,CMOS,105,hybrid,IP67,IK10,true,2021,0\nbeta-cam,Beta,Shared,bullet,4K,8,CMOS,105,hybrid,IP67,IK10,true,2021,0\n",
            None,
        );

        let database = CameraDatabase::from_archive(&archive).unwrap();

        assert!(matches!(
            database.match_camera("Unknown", "Shared"),
            CameraMatch::Ambiguous
        ));
        assert!(matches!(
            database.match_camera("Alpha", "Shared"),
            CameraMatch::Exact(_)
        ));
    }

    #[test]
    fn renders_template_hints_without_credentials() {
        let archive = archive_with(
            vec![camera_json(
                "reolink-rlc-811a",
                "Reolink",
                "RLC-811A",
                Vec::new(),
            )],
            "id,brand,model,type,resolution_label,megapixels,sensor,field_of_view_deg,night_vision_type,ip_rating,ik_rating,two_way_audio,release_year,community_notes_count\nreolink-rlc-811a,Reolink,RLC-811A,bullet,4K,8,CMOS,105,hybrid,IP67,IK10,true,2021,0\n",
            None,
        );
        let database = CameraDatabase::from_archive(&archive).unwrap();

        let hints = database
            .stream_hints("reolink-rlc-811a", "192.0.2.77".parse().unwrap())
            .unwrap();

        assert_eq!(hints.main.as_deref(), Some("rtsp://192.0.2.77:554/main"));
        assert_eq!(hints.sub.as_deref(), Some("rtsp://192.0.2.77:554/sub"));
    }

    #[test]
    fn rejects_archive_members_that_do_not_match_metadata_hashes() {
        let archive = archive_with(
            vec![camera_json(
                "reolink-rlc-811a",
                "Reolink",
                "RLC-811A",
                Vec::new(),
            )],
            "id,brand,model,type,resolution_label,megapixels,sensor,field_of_view_deg,night_vision_type,ip_rating,ik_rating,two_way_audio,release_year,community_notes_count\nreolink-rlc-811a,Reolink,RLC-811A,bullet,4K,8,CMOS,105,hybrid,IP67,IK10,true,2021,0\n",
            Some("00".repeat(32)),
        );

        let error = CameraDatabase::from_archive(&archive).unwrap_err();

        assert!(error.to_string().contains("checksum"));
    }

    fn camera_json(id: &str, brand: &str, model: &str, aliases: Vec<&str>) -> serde_json::Value {
        json!({
            "id": id,
            "brand": brand,
            "model": model,
            "aliases": aliases,
            "type": "bullet",
            "resolution": { "megapixels": 8, "label": "4K" },
            "protocols": ["onvif", "rtsp"],
            "video": {
                "codecs": ["H.265", "H.264"],
                "streams": [
                    { "name": "main", "resolution": "3840x2160", "fps": 25, "codec": "H.265" },
                    { "name": "sub", "resolution": "640x360", "fps": 10, "codec": "H.264" }
                ]
            },
            "configs": {
                "frigate": {
                    "rtsp_url_template": "rtsp://{user}:{pass}@{ip}:554/main",
                    "best_substream": "rtsp://{user}:{pass}@{ip}:554/sub"
                }
            },
            "sources": ["https://example.com/camera"]
        })
    }

    fn archive_with(
        cameras: Vec<serde_json::Value>,
        csv: &str,
        json_hash_override: Option<String>,
    ) -> Vec<u8> {
        let json = serde_json::to_vec(&cameras).unwrap();
        let metadata = json!({
            "schema_version": 1,
            "version": "2.1.0",
            "tag": "v2.1.0",
            "generated_at": "2026-08-22T00:00:00Z",
            "camera_count": cameras.len(),
            "files": [
                {
                    "name": JSON_FILE,
                    "bytes": json.len(),
                    "sha256": json_hash_override.unwrap_or_else(|| sha256(&json))
                },
                {
                    "name": CSV_FILE,
                    "bytes": csv.len(),
                    "sha256": sha256(csv.as_bytes())
                }
            ]
        });
        let metadata = serde_json::to_vec(&metadata).unwrap();
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, contents) in [
            (JSON_FILE, json.as_slice()),
            (CSV_FILE, csv.as_bytes()),
            (METADATA_FILE, metadata.as_slice()),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn sha256(contents: &[u8]) -> String {
        encode_lower_hex(Sha256::digest(contents))
    }
}
