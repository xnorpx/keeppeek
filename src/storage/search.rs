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
    pub start_time_ms: i64,
    pub end_time_ms: Option<i64>,
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
                    zone: None,
                    thumbnail_filename: None,
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
