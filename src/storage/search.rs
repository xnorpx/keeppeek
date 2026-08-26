//! Indexed event metadata and model-scoped semantic search with lazy encoded previews.

use crate::storage::{EncodedEventKeyframe, EventKeyframeLocation, RecordingCatalogHandle};
use serde::{Deserialize, Serialize};

/// Default context included before an event start.
pub const DEFAULT_PREVIEW_BEFORE_MS: u64 = 5_000;
/// Default context included after an event end.
pub const DEFAULT_PREVIEW_AFTER_MS: u64 = 10_000;
const MAX_SEARCH_TERMS: usize = 64;
const MAX_TERM_BYTES: usize = 256;
const MAX_MODEL_ID_BYTES: usize = 128;
const MAX_EMBEDDING_DIMENSIONS: usize = 4_096;
const MAX_FILTER_VALUES: usize = 64;
const MAX_PAGE_SIZE: u32 = 128;
const MAX_SEMANTIC_WINDOW_MS: i64 = 31 * 86_400_000;
const MAX_PREVIEW_WINDOW_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// A structured field attached to a searchable event.
pub enum EventSearchField {
    EventType,
    FaceName,
    ObjectClass,
    Text,
}

impl EventSearchField {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EventType => "event_type",
            Self::FaceName => "face_name",
            Self::ObjectClass => "object_class",
            Self::Text => "text",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "event_type" => Some(Self::EventType),
            "face_name" => Some(Self::FaceName),
            "object_class" => Some(Self::ObjectClass),
            "text" => Some(Self::Text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One producer-supplied structured search term.
pub struct EventSearchTerm {
    pub field: EventSearchField,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
/// A dense embedding whose model ID identifies its vector space and version.
pub struct EventEmbedding {
    pub model_id: String,
    pub values: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A bounded case-insensitive prefix query over structured event terms.
pub struct EventTextSearchQuery {
    pub query: String,
    pub field: Option<EventSearchField>,
    pub source_id: Option<String>,
    pub stream_id: String,
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub preview_before_ms: u64,
    pub preview_after_ms: u64,
    pub page_size: u32,
    pub page_token: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Selects events by supported image-attachment presence.
pub enum EventImageFilter {
    #[default]
    Any,
    WithImage,
    WithoutImage,
}

#[derive(Debug, Clone, PartialEq)]
/// A bounded newest-first query over event metadata.
pub struct EventMetadataQuery {
    pub event_ids: Vec<String>,
    pub source_ids: Vec<String>,
    pub event_types: Vec<String>,
    pub origins: Vec<crate::storage::metadata::EventSource>,
    pub zones: Vec<String>,
    pub minimum_confidence: Option<f64>,
    pub image: EventImageFilter,
    pub text: Option<String>,
    pub stream_id: String,
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub preview_before_ms: u64,
    pub preview_after_ms: u64,
    pub page_size: u32,
    pub page_token: Option<String>,
}

impl EventMetadataQuery {
    /// Creates an unfiltered query with the default preview interval and a 50-hit page.
    pub fn new(stream_id: impl Into<String>, start_time_ms: i64, end_time_ms: i64) -> Self {
        Self {
            event_ids: Vec::new(),
            source_ids: Vec::new(),
            event_types: Vec::new(),
            origins: Vec::new(),
            zones: Vec::new(),
            minimum_confidence: None,
            image: EventImageFilter::Any,
            text: None,
            stream_id: stream_id.into(),
            start_time_ms,
            end_time_ms,
            preview_before_ms: DEFAULT_PREVIEW_BEFORE_MS,
            preview_after_ms: DEFAULT_PREVIEW_AFTER_MS,
            page_size: 50,
            page_token: None,
        }
    }
}

impl EventTextSearchQuery {
    /// Creates a query with the default preview interval and a 50-hit page.
    pub fn new(
        query: impl Into<String>,
        stream_id: impl Into<String>,
        start_time_ms: i64,
        end_time_ms: i64,
    ) -> Self {
        Self {
            query: query.into(),
            field: None,
            source_id: None,
            stream_id: stream_id.into(),
            start_time_ms,
            end_time_ms,
            preview_before_ms: DEFAULT_PREVIEW_BEFORE_MS,
            preview_after_ms: DEFAULT_PREVIEW_AFTER_MS,
            page_size: 50,
            page_token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
/// A bounded exact cosine-similarity query over one embedding model.
pub struct EventSemanticSearchQuery {
    pub embedding: EventEmbedding,
    pub source_id: Option<String>,
    pub stream_id: String,
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub preview_before_ms: u64,
    pub preview_after_ms: u64,
    pub page_size: u32,
    pub page_token: Option<String>,
}

impl EventSemanticSearchQuery {
    /// Creates a query with the default preview interval and a 50-hit page.
    pub fn new(
        embedding: EventEmbedding,
        stream_id: impl Into<String>,
        start_time_ms: i64,
        end_time_ms: i64,
    ) -> Self {
        Self {
            embedding,
            source_id: None,
            stream_id: stream_id.into(),
            start_time_ms,
            end_time_ms,
            preview_before_ms: DEFAULT_PREVIEW_BEFORE_MS,
            preview_after_ms: DEFAULT_PREVIEW_AFTER_MS,
            page_size: 50,
            page_token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
/// Event metadata and immutable keyframe descriptors for one preview interval.
pub struct EventSearchHit {
    pub event_id: String,
    pub source_id: String,
    pub event_type: String,
    pub origin: crate::storage::metadata::EventSource,
    pub start_time_ms: i64,
    pub end_time_ms: Option<i64>,
    pub confidence: Option<f64>,
    pub bbox: Option<[f32; 4]>,
    pub zone: Option<String>,
    pub text: Option<String>,
    pub has_image_attachment: bool,
    pub score: Option<f64>,
    pub(crate) semantic_distance: Option<f64>,
    pub preview_start_ms: i64,
    pub preview_end_ms: i64,
    pub keyframes: Vec<EventKeyframeLocation>,
    pub keyframes_truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
/// One metadata page; `next_page_token` can be applied to the next query.
pub struct EventSearchPage {
    pub hits: Vec<EventSearchHit>,
    pub next_page_token: Option<String>,
    pub candidates_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Encoded keyframes read for one search hit's preview interval.
pub struct EncodedEventPreview {
    pub event_id: String,
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub keyframes: Vec<EncodedEventKeyframe>,
}

#[derive(Clone)]
/// Search and lazy-preview access over a recording catalog.
pub struct EventSearch {
    catalog: RecordingCatalogHandle,
}

impl EventSearch {
    /// Creates a search facade over the catalog handle.
    pub const fn new(catalog: RecordingCatalogHandle) -> Self {
        Self { catalog }
    }

    /// Replaces producer-supplied terms while preserving the automatic event-type term.
    pub fn replace_terms(&self, event_id: &str, terms: &[EventSearchTerm]) -> anyhow::Result<()> {
        validate_event_id(event_id)?;
        if terms.len() > MAX_SEARCH_TERMS {
            anyhow::bail!("event search term count exceeds {MAX_SEARCH_TERMS}");
        }
        let normalized = terms
            .iter()
            .map(|term| {
                if term.field == EventSearchField::EventType {
                    anyhow::bail!("event type search terms are catalog-owned");
                }
                let value = term.value.split_whitespace().collect::<Vec<_>>().join(" ");
                normalize_search_text(&value)?;
                Ok(EventSearchTerm {
                    field: term.field,
                    value,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.catalog
            .replace_event_search_terms(event_id, normalized)
    }

    /// Creates or replaces an event embedding for its model ID.
    pub fn set_embedding(&self, event_id: &str, embedding: EventEmbedding) -> anyhow::Result<()> {
        validate_event_id(event_id)?;
        validate_embedding(&embedding)?;
        self.catalog.set_event_embedding(event_id, embedding)
    }

    /// Browses newest-first event metadata with composable filters.
    pub fn search_metadata(
        &self,
        mut query: EventMetadataQuery,
    ) -> anyhow::Result<EventSearchPage> {
        normalize_metadata_query(&mut query)?;
        self.catalog.search_event_metadata(query)
    }

    /// Searches structured terms and returns metadata plus lazy keyframe descriptors.
    pub fn search_text(&self, mut query: EventTextSearchQuery) -> anyhow::Result<EventSearchPage> {
        query.query = normalize_search_text(&query.query)?;
        validate_search_window(
            &query.stream_id,
            query.start_time_ms,
            query.end_time_ms,
            query.preview_before_ms,
            query.preview_after_ms,
            query.page_size,
        )?;
        if query.end_time_ms.saturating_sub(query.start_time_ms) > MAX_SEMANTIC_WINDOW_MS {
            anyhow::bail!("event text search window exceeds 31 days");
        }
        self.catalog.search_event_text(query)
    }

    /// Ranks compatible embeddings by exact cosine similarity.
    pub fn search_semantic(
        &self,
        query: EventSemanticSearchQuery,
    ) -> anyhow::Result<EventSearchPage> {
        validate_embedding(&query.embedding)?;
        validate_search_window(
            &query.stream_id,
            query.start_time_ms,
            query.end_time_ms,
            query.preview_before_ms,
            query.preview_after_ms,
            query.page_size,
        )?;
        if query.end_time_ms.saturating_sub(query.start_time_ms) > MAX_SEMANTIC_WINDOW_MS {
            anyhow::bail!("semantic event search window exceeds 31 days");
        }
        self.catalog.search_event_semantic(query)
    }

    /// Reads every encoded keyframe referenced by one hit.
    pub fn read_preview(&self, hit: &EventSearchHit) -> anyhow::Result<EncodedEventPreview> {
        let lookup = crate::storage::EventKeyframeLookup::new(self.catalog.clone());
        let keyframes = hit
            .keyframes
            .iter()
            .map(|location| lookup.read_location(location.clone()))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(EncodedEventPreview {
            event_id: hit.event_id.clone(),
            start_time_ms: hit.preview_start_ms,
            end_time_ms: hit.preview_end_ms,
            keyframes,
        })
    }
}

pub(crate) fn normalize_search_text(value: &str) -> anyhow::Result<String> {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if normalized.is_empty() {
        anyhow::bail!("event search text must not be empty");
    }
    if normalized.len() > MAX_TERM_BYTES {
        anyhow::bail!("event search text exceeds {MAX_TERM_BYTES} UTF-8 bytes");
    }
    Ok(normalized)
}

fn normalize_metadata_query(query: &mut EventMetadataQuery) -> anyhow::Result<()> {
    validate_search_window(
        &query.stream_id,
        query.start_time_ms,
        query.end_time_ms,
        query.preview_before_ms,
        query.preview_after_ms,
        query.page_size,
    )?;
    if query.end_time_ms.saturating_sub(query.start_time_ms) > MAX_SEMANTIC_WINDOW_MS {
        anyhow::bail!("event metadata search window exceeds 31 days");
    }
    if query
        .minimum_confidence
        .is_some_and(|confidence| !confidence.is_finite() || !(0.0..=1.0).contains(&confidence))
    {
        anyhow::bail!("event minimum confidence must be between zero and one");
    }
    normalize_source_ids(&mut query.source_ids)?;
    normalize_event_ids(&mut query.event_ids)?;
    normalize_filter_values(&mut query.event_types, "event type")?;
    normalize_filter_values(&mut query.zones, "zone")?;
    if query.origins.len() > MAX_FILTER_VALUES {
        anyhow::bail!("event origin filter count exceeds {MAX_FILTER_VALUES}");
    }
    query
        .origins
        .sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
    query.origins.dedup();
    query.text = query
        .text
        .as_deref()
        .map(normalize_search_text)
        .transpose()?;
    Ok(())
}

fn normalize_event_ids(values: &mut Vec<String>) -> anyhow::Result<()> {
    if values.len() > MAX_FILTER_VALUES {
        anyhow::bail!("event ID filter count exceeds {MAX_FILTER_VALUES}");
    }
    for value in &mut *values {
        *value = value.trim().to_owned();
        if value.is_empty() || value.len() > MAX_TERM_BYTES {
            anyhow::bail!("event ID filter must contain 1 to {MAX_TERM_BYTES} UTF-8 bytes");
        }
    }
    values.sort_unstable();
    values.dedup();
    Ok(())
}

fn normalize_source_ids(values: &mut Vec<String>) -> anyhow::Result<()> {
    if values.len() > MAX_FILTER_VALUES {
        anyhow::bail!("event source filter count exceeds {MAX_FILTER_VALUES}");
    }
    for value in &mut *values {
        *value = value.trim().to_owned();
        if value.is_empty() || value.len() > MAX_TERM_BYTES {
            anyhow::bail!("event source filter must contain 1 to {MAX_TERM_BYTES} UTF-8 bytes");
        }
    }
    values.sort_unstable();
    values.dedup();
    Ok(())
}

fn normalize_filter_values(values: &mut Vec<String>, label: &str) -> anyhow::Result<()> {
    if values.len() > MAX_FILTER_VALUES {
        anyhow::bail!("event {label} filter count exceeds {MAX_FILTER_VALUES}");
    }
    for value in &mut *values {
        *value = normalize_search_text(value)?;
    }
    values.sort_unstable();
    values.dedup();
    Ok(())
}

pub(crate) fn validate_embedding(embedding: &EventEmbedding) -> anyhow::Result<()> {
    let model_id = embedding.model_id.trim();
    if model_id.is_empty() || model_id.len() > MAX_MODEL_ID_BYTES {
        anyhow::bail!("embedding model ID must contain 1 to {MAX_MODEL_ID_BYTES} UTF-8 bytes");
    }
    if embedding.values.is_empty() || embedding.values.len() > MAX_EMBEDDING_DIMENSIONS {
        anyhow::bail!("embedding must contain 1 to {MAX_EMBEDDING_DIMENSIONS} dimensions");
    }
    if embedding.values.iter().any(|value| !value.is_finite()) {
        anyhow::bail!("embedding values must be finite");
    }
    let magnitude = embedding
        .values
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>();
    if magnitude <= f64::EPSILON {
        anyhow::bail!("embedding must have a non-zero magnitude");
    }
    Ok(())
}

fn validate_event_id(event_id: &str) -> anyhow::Result<()> {
    if event_id.is_empty() {
        anyhow::bail!("event ID must not be empty");
    }
    Ok(())
}

fn validate_search_window(
    stream_id: &str,
    start_time_ms: i64,
    end_time_ms: i64,
    preview_before_ms: u64,
    preview_after_ms: u64,
    page_size: u32,
) -> anyhow::Result<()> {
    if stream_id.is_empty() {
        anyhow::bail!("event search stream ID must not be empty");
    }
    if start_time_ms >= end_time_ms {
        anyhow::bail!("event search start must precede its end");
    }
    if preview_before_ms.saturating_add(preview_after_ms) > MAX_PREVIEW_WINDOW_MS {
        anyhow::bail!("event preview window exceeds 60 seconds");
    }
    if page_size == 0 || page_size > MAX_PAGE_SIZE {
        anyhow::bail!("event search page size must be between 1 and {MAX_PAGE_SIZE}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_query_normalizes_filters_and_rejects_invalid_limits() {
        let mut query = EventMetadataQuery::new("main", 0, 40_000);
        query.source_ids = vec![" Front Door ".to_owned(), "Front Door".to_owned()];
        query.event_types = vec![" Person ".to_owned(), "person".to_owned()];
        query.text = Some("  Alice   Example ".to_owned());

        normalize_metadata_query(&mut query).unwrap();

        assert_eq!(query.source_ids, ["Front Door"]);
        assert_eq!(query.event_types, ["person"]);
        assert_eq!(query.text.as_deref(), Some("alice example"));

        query.minimum_confidence = Some(f64::NAN);
        assert_eq!(
            normalize_metadata_query(&mut query)
                .unwrap_err()
                .to_string(),
            "event minimum confidence must be between zero and one"
        );

        query.minimum_confidence = None;
        query.zones = (0..=MAX_FILTER_VALUES)
            .map(|index| index.to_string())
            .collect();
        assert_eq!(
            normalize_metadata_query(&mut query)
                .unwrap_err()
                .to_string(),
            "event zone filter count exceeds 64"
        );
    }
    use crate::storage::{
        CatalogFragment, CatalogKeyframe, CatalogRecording, RecordingCatalog,
        metadata::{EventSource, TimelineEvent},
    };

    #[test]
    fn searches_event_metadata_and_semantics_with_lazy_encoded_previews() {
        let root =
            std::env::temp_dir().join(format!("keeppeek-event-search-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&root).unwrap();
        let recording_path = root.join("recording.mp4");
        std::fs::write(&recording_path, b"header-encoded-keyframe-trailer").unwrap();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "recording-1".to_owned(),
                stream_id: "front-door/main".to_owned(),
                source_id: Some("front-door".to_owned()),
                logical_stream_id: Some("main".to_owned()),
                started_at_ms: 10_000,
                ended_at_ms: Some(30_000),
                path: recording_path.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 7,
                finalized: true,
            })
            .unwrap();
        handle
            .insert_fragment_with_keyframe(
                CatalogFragment {
                    recording_id: "recording-1".to_owned(),
                    sequence: 1,
                    start_ms: 10_000,
                    duration_ms: 20_000,
                    byte_offset: 7,
                    byte_len: 24,
                    random_access: true,
                },
                CatalogKeyframe {
                    recording_id: "recording-1".to_owned(),
                    fragment_sequence: 1,
                    byte_offset: 7,
                    byte_len: 16,
                },
            )
            .unwrap();
        for (id, kind, start_time_ms) in [
            ("event-alice", "face", 15_000),
            ("event-vehicle", "object", 17_000),
        ] {
            handle
                .insert_event(TimelineEvent {
                    id: id.to_owned(),
                    camera_id: "front-door".to_owned(),
                    stream: Some("main".to_owned()),
                    source: EventSource::KeepPeek,
                    kind: kind.to_owned(),
                    start_time_ms,
                    end_time_ms: Some(start_time_ms + 1_000),
                    confidence: Some(0.9),
                    bbox: None,
                    zone: (id == "event-alice").then(|| "Porch".to_owned()),
                    thumbnail_filename: (id == "event-alice").then(|| "event-alice.jpg".to_owned()),
                })
                .unwrap();
        }
        handle
            .insert_event(TimelineEvent {
                id: "event-sub".to_owned(),
                camera_id: "front-door".to_owned(),
                stream: Some("sub".to_owned()),
                source: EventSource::KeepPeek,
                kind: "face".to_owned(),
                start_time_ms: 19_000,
                end_time_ms: Some(20_000),
                confidence: Some(0.8),
                bbox: None,
                zone: None,
                thumbnail_filename: None,
            })
            .unwrap();

        let search = EventSearch::new(handle.clone());
        let event_type_error = search
            .replace_terms(
                "event-alice",
                &[EventSearchTerm {
                    field: EventSearchField::EventType,
                    value: "vehicle".to_owned(),
                }],
            )
            .unwrap_err();
        assert_eq!(
            event_type_error.to_string(),
            "event type search terms are catalog-owned"
        );
        search
            .replace_terms(
                "event-alice",
                &[
                    EventSearchTerm {
                        field: EventSearchField::FaceName,
                        value: "Alice Example".to_owned(),
                    },
                    EventSearchTerm {
                        field: EventSearchField::ObjectClass,
                        value: "Person".to_owned(),
                    },
                    EventSearchTerm {
                        field: EventSearchField::Text,
                        value: "match".to_owned(),
                    },
                ],
            )
            .unwrap();
        search
            .replace_terms(
                "event-vehicle",
                &[
                    EventSearchTerm {
                        field: EventSearchField::ObjectClass,
                        value: "Delivery Van".to_owned(),
                    },
                    EventSearchTerm {
                        field: EventSearchField::Text,
                        value: "match".to_owned(),
                    },
                ],
            )
            .unwrap();
        search
            .replace_terms(
                "event-sub",
                &[EventSearchTerm {
                    field: EventSearchField::FaceName,
                    value: "Alice Sub".to_owned(),
                }],
            )
            .unwrap();

        let mut metadata_query = EventMetadataQuery::new("main", 0, 40_000);
        metadata_query.event_ids = vec!["event-alice".to_owned()];
        metadata_query.source_ids = vec!["front-door".to_owned()];
        metadata_query.event_types = vec!["FACE".to_owned()];
        metadata_query.origins = vec![EventSource::KeepPeek];
        metadata_query.zones = vec![" porch ".to_owned()];
        metadata_query.minimum_confidence = Some(0.85);
        metadata_query.image = EventImageFilter::WithImage;
        metadata_query.text = Some("alice".to_owned());
        let metadata_page = search.search_metadata(metadata_query).unwrap();
        assert_eq!(metadata_page.hits.len(), 1);
        assert_eq!(metadata_page.hits[0].event_id, "event-alice");
        assert_eq!(metadata_page.hits[0].origin, EventSource::KeepPeek);
        assert_eq!(metadata_page.hits[0].confidence, Some(0.9));
        assert_eq!(metadata_page.hits[0].zone.as_deref(), Some("Porch"));
        assert_eq!(metadata_page.hits[0].text.as_deref(), Some("match"));
        assert!(metadata_page.hits[0].has_image_attachment);
        assert_eq!(metadata_page.hits[0].keyframes.len(), 1);

        let matching_ids = |query: EventMetadataQuery| {
            search
                .search_metadata(query)
                .unwrap()
                .hits
                .into_iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>()
        };
        let mut event_id_query = EventMetadataQuery::new("main", 0, 40_000);
        event_id_query.event_ids = vec!["event-alice".to_owned()];
        assert_eq!(matching_ids(event_id_query), ["event-alice"]);
        let mut source_query = EventMetadataQuery::new("main", 0, 40_000);
        source_query.source_ids = vec!["front-door".to_owned()];
        assert_eq!(matching_ids(source_query), ["event-vehicle", "event-alice"]);
        let mut type_query = EventMetadataQuery::new("main", 0, 40_000);
        type_query.event_types = vec!["face".to_owned()];
        assert_eq!(matching_ids(type_query), ["event-alice"]);
        let mut origin_query = EventMetadataQuery::new("main", 0, 40_000);
        origin_query.origins = vec![EventSource::KeepPeek];
        assert_eq!(matching_ids(origin_query), ["event-vehicle", "event-alice"]);
        let mut zone_query = EventMetadataQuery::new("main", 0, 40_000);
        zone_query.zones = vec!["porch".to_owned()];
        assert_eq!(matching_ids(zone_query), ["event-alice"]);
        let mut confidence_query = EventMetadataQuery::new("main", 0, 40_000);
        confidence_query.minimum_confidence = Some(0.9);
        assert_eq!(
            matching_ids(confidence_query),
            ["event-vehicle", "event-alice"]
        );
        let mut with_image_query = EventMetadataQuery::new("main", 0, 40_000);
        with_image_query.image = EventImageFilter::WithImage;
        assert_eq!(matching_ids(with_image_query), ["event-alice"]);
        let mut without_image_query = EventMetadataQuery::new("main", 0, 40_000);
        without_image_query.image = EventImageFilter::WithoutImage;
        assert_eq!(matching_ids(without_image_query), ["event-vehicle"]);
        let mut text_metadata_query = EventMetadataQuery::new("main", 0, 40_000);
        text_metadata_query.text = Some("delivery".to_owned());
        assert_eq!(matching_ids(text_metadata_query), ["event-vehicle"]);
        let time_query = EventMetadataQuery::new("main", 16_000, 18_000);
        assert_eq!(matching_ids(time_query), ["event-vehicle"]);

        let mut first_metadata_query = EventMetadataQuery::new("main", 0, 40_000);
        first_metadata_query.page_size = 1;
        let first_metadata_page = search
            .search_metadata(first_metadata_query.clone())
            .unwrap();
        assert_eq!(first_metadata_page.hits[0].event_id, "event-vehicle");
        assert!(first_metadata_page.next_page_token.is_some());
        handle
            .insert_event(TimelineEvent {
                id: "event-metadata-new".to_owned(),
                camera_id: "front-door".to_owned(),
                stream: Some("main".to_owned()),
                source: EventSource::Camera,
                kind: "motion".to_owned(),
                start_time_ms: 16_000,
                end_time_ms: None,
                confidence: None,
                bbox: None,
                zone: None,
                thumbnail_filename: None,
            })
            .unwrap();
        let mut camera_origin_query = EventMetadataQuery::new("main", 0, 40_000);
        camera_origin_query.origins = vec![EventSource::Camera];
        assert_eq!(matching_ids(camera_origin_query), ["event-metadata-new"]);
        first_metadata_query.page_token = first_metadata_page.next_page_token.clone();
        let second_metadata_page = search.search_metadata(first_metadata_query).unwrap();
        assert_eq!(second_metadata_page.hits[0].event_id, "event-alice");
        assert_eq!(second_metadata_page.next_page_token, None);

        let mut mismatched_metadata_query = EventMetadataQuery::new("main", 0, 40_000);
        mismatched_metadata_query.page_size = 1;
        mismatched_metadata_query.event_types = vec!["face".to_owned()];
        mismatched_metadata_query.page_token = first_metadata_page.next_page_token;
        assert_eq!(
            search
                .search_metadata(mismatched_metadata_query)
                .unwrap_err()
                .to_string(),
            "event search page token does not match the metadata query"
        );
        search
            .set_embedding(
                "event-alice",
                EventEmbedding {
                    model_id: "vision-embedding".to_owned(),
                    values: vec![1.0, 0.0, 0.0],
                },
            )
            .unwrap();
        search
            .set_embedding(
                "event-vehicle",
                EventEmbedding {
                    model_id: "vision-embedding".to_owned(),
                    values: vec![0.0, 1.0, 0.0],
                },
            )
            .unwrap();
        search
            .set_embedding(
                "event-sub",
                EventEmbedding {
                    model_id: "vision-embedding".to_owned(),
                    values: vec![1.0, 0.0, 0.0],
                },
            )
            .unwrap();

        let mut text_query = EventTextSearchQuery::new("  ALICE ", "main", 0, 40_000);
        text_query.field = Some(EventSearchField::FaceName);
        let text_hits = search.search_text(text_query).unwrap();
        assert_eq!(text_hits.hits.len(), 1);
        assert_eq!(text_hits.hits[0].event_id, "event-alice");
        assert_eq!(text_hits.hits[0].preview_start_ms, 10_000);
        assert_eq!(text_hits.hits[0].preview_end_ms, 26_000);
        assert_eq!(text_hits.hits[0].keyframes.len(), 1);
        assert_eq!(text_hits.next_page_token, None);
        let preview = search.read_preview(&text_hits.hits[0]).unwrap();
        assert_eq!(preview.keyframes.len(), 1);
        assert_eq!(preview.keyframes[0].bytes, b"encoded-keyframe");

        let mut sub_query = EventTextSearchQuery::new("alice", "sub", 0, 40_000);
        sub_query.field = Some(EventSearchField::FaceName);
        let sub_hits = search.search_text(sub_query).unwrap();
        assert_eq!(sub_hits.hits.len(), 1);
        assert_eq!(sub_hits.hits[0].event_id, "event-sub");

        let event_type_hits = search
            .search_text(EventTextSearchQuery::new("fac", "main", 0, 40_000))
            .unwrap();
        assert_eq!(event_type_hits.hits.len(), 1);
        assert_eq!(event_type_hits.hits[0].event_id, "event-alice");

        let mut first_text_page = EventTextSearchQuery::new("match", "main", 0, 40_000);
        first_text_page.field = Some(EventSearchField::Text);
        first_text_page.page_size = 1;
        let first_text_page = search.search_text(first_text_page).unwrap();
        assert_eq!(first_text_page.hits[0].event_id, "event-vehicle");
        assert!(first_text_page.next_page_token.is_some());
        handle
            .insert_event(TimelineEvent {
                id: "event-between-pages".to_owned(),
                camera_id: "front-door".to_owned(),
                stream: Some("main".to_owned()),
                source: EventSource::KeepPeek,
                kind: "object".to_owned(),
                start_time_ms: 16_000,
                end_time_ms: None,
                confidence: None,
                bbox: None,
                zone: None,
                thumbnail_filename: None,
            })
            .unwrap();
        search
            .replace_terms(
                "event-between-pages",
                &[EventSearchTerm {
                    field: EventSearchField::Text,
                    value: "match".to_owned(),
                }],
            )
            .unwrap();
        let mut second_text_query = EventTextSearchQuery::new("match", "main", 0, 40_000);
        second_text_query.field = Some(EventSearchField::Text);
        second_text_query.page_size = 1;
        second_text_query.page_token = first_text_page.next_page_token;
        let second_text_page = search.search_text(second_text_query).unwrap();
        assert_eq!(second_text_page.hits[0].event_id, "event-alice");

        let mut mutable_text_query = EventTextSearchQuery::new("match", "main", 0, 40_000);
        mutable_text_query.field = Some(EventSearchField::Text);
        mutable_text_query.page_size = 1;
        let mutable_text_page = search.search_text(mutable_text_query.clone()).unwrap();
        search
            .replace_terms(
                "event-alice",
                &[EventSearchTerm {
                    field: EventSearchField::Text,
                    value: "match".to_owned(),
                }],
            )
            .unwrap();
        mutable_text_query.page_token = mutable_text_page.next_page_token;
        let mutation_error = search.search_text(mutable_text_query).unwrap_err();
        assert_eq!(
            mutation_error.to_string(),
            "event search snapshot changed; restart the query"
        );

        let semantic_hits = search
            .search_semantic(EventSemanticSearchQuery::new(
                EventEmbedding {
                    model_id: "vision-embedding".to_owned(),
                    values: vec![0.9, 0.1, 0.0],
                },
                "main",
                0,
                40_000,
            ))
            .unwrap();
        assert_eq!(semantic_hits.hits.len(), 2);
        assert_eq!(semantic_hits.hits[0].event_id, "event-alice");
        assert!(semantic_hits.hits[0].score.unwrap() > semantic_hits.hits[1].score.unwrap());

        let mut first_page_query = EventSemanticSearchQuery::new(
            EventEmbedding {
                model_id: "vision-embedding".to_owned(),
                values: vec![0.9, 0.1, 0.0],
            },
            "main",
            0,
            40_000,
        );
        first_page_query.page_size = 1;
        let first_page = search.search_semantic(first_page_query.clone()).unwrap();
        assert_eq!(first_page.hits.len(), 1);
        assert!(first_page.next_page_token.is_some());
        first_page_query.page_token = first_page.next_page_token;
        let second_page = search.search_semantic(first_page_query).unwrap();
        assert_eq!(second_page.hits.len(), 1);
        assert_ne!(first_page.hits[0].event_id, second_page.hits[0].event_id);
        assert_eq!(second_page.next_page_token, None);

        let mut mutable_semantic_query = EventSemanticSearchQuery::new(
            EventEmbedding {
                model_id: "vision-embedding".to_owned(),
                values: vec![0.9, 0.1, 0.0],
            },
            "main",
            0,
            40_000,
        );
        mutable_semantic_query.page_size = 1;
        let mutable_semantic_page = search
            .search_semantic(mutable_semantic_query.clone())
            .unwrap();
        search
            .set_embedding(
                "event-vehicle",
                EventEmbedding {
                    model_id: "vision-embedding".to_owned(),
                    values: vec![0.25, 0.75, 0.0],
                },
            )
            .unwrap();
        mutable_semantic_query.page_token = mutable_semantic_page.next_page_token;
        let mutation_error = search.search_semantic(mutable_semantic_query).unwrap_err();
        assert_eq!(
            mutation_error.to_string(),
            "event search snapshot changed; restart the query"
        );

        let no_model_hits = search
            .search_semantic(EventSemanticSearchQuery::new(
                EventEmbedding {
                    model_id: "different-model".to_owned(),
                    values: vec![0.9, 0.1, 0.0],
                },
                "main",
                0,
                40_000,
            ))
            .unwrap();
        assert!(no_model_hits.hits.is_empty());

        for index in 0..11 {
            let event_id = format!("bounded-{index:02}");
            handle
                .insert_event(TimelineEvent {
                    id: event_id.clone(),
                    camera_id: "front-door".to_owned(),
                    stream: Some("main".to_owned()),
                    source: EventSource::KeepPeek,
                    kind: "object".to_owned(),
                    start_time_ms: 18_000 + index,
                    end_time_ms: None,
                    confidence: None,
                    bbox: None,
                    zone: None,
                    thumbnail_filename: None,
                })
                .unwrap();
            search
                .set_embedding(
                    &event_id,
                    EventEmbedding {
                        model_id: "bounded-model".to_owned(),
                        values: vec![1.0, index as f32 + 1.0, 0.0],
                    },
                )
                .unwrap();
        }
        let bounded = search
            .search_semantic(EventSemanticSearchQuery::new(
                EventEmbedding {
                    model_id: "bounded-model".to_owned(),
                    values: vec![1.0, 1.0, 0.0],
                },
                "main",
                0,
                40_000,
            ))
            .unwrap();
        assert_eq!(bounded.hits.len(), 10);
        assert!(bounded.candidates_truncated);

        let text_window_error = search
            .search_text(EventTextSearchQuery::new(
                "alice",
                "main",
                i64::MIN,
                i64::MAX,
            ))
            .unwrap_err();
        assert_eq!(
            text_window_error.to_string(),
            "event text search window exceeds 31 days"
        );

        drop(search);
        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
}
