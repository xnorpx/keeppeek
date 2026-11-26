pub mod adts;
pub mod analytics;
pub mod archive;
#[doc(hidden)]
pub mod benchmark;
pub mod catalog;
pub mod demand;
pub mod engine;
pub(crate) mod event_recording;
pub mod events;
pub mod frame;
pub(crate) mod health;
pub mod identity;
pub mod layout;
pub mod long_term;
pub mod media_objects;
pub mod medium_term;
pub mod metadata;
pub mod nal;
pub mod playback;
mod pre_record;
mod recording_policy;
pub mod retention;
pub(crate) mod safety;
pub(crate) mod sample_index;
pub mod search;
pub mod segment;
pub mod short_term;
#[cfg(test)]
mod test_media;
pub mod volumes;

const RECORDING_METADATA_BYTES_MAX: u64 = 8 * 1024 * 1024;
const RECORDING_BOX_BYTES_MAX: u64 = 1024 * 1024;
const RECORDING_BOX_COUNT_MAX: usize = 16_384;
const RECORDING_FRAGMENT_COUNT_MAX: usize = 4_096;

// Rotate at the next keyframe once three quarters of a recovery budget is used.
// The remaining quarter usually accommodates the entire next GOP at ordinary camera rates.
const ROTATION_RESERVE_DIVISOR: u64 = 4;
const fn recording_metadata_rotation_due(usage: mp4::FragmentedMetadataUsage) -> bool {
    const fn near_limit(used: u64, limit: u64) -> bool {
        used >= limit - limit / ROTATION_RESERVE_DIVISOR
    }
    near_limit(usage.bytes, RECORDING_METADATA_BYTES_MAX)
        || near_limit(usage.boxes, RECORDING_BOX_COUNT_MAX as u64)
        || near_limit(usage.fragments as u64, RECORDING_FRAGMENT_COUNT_MAX as u64)
}

pub use catalog::{
    CatalogEventKeyframeLink, CatalogFragment, CatalogKeyframe, CatalogMediaFragment,
    CatalogMediaObjectLocation, CatalogRecording, EventKeyframeLocation, RecordingCatalog,
    RecordingCatalogHandle,
};
pub use demand::{RecordingDemand, RecordingDemandGuard};
pub(crate) use engine::RecordingConfiguration;
pub use engine::{StorageConfig, StorageEngine, StorageHandle};
pub use events::EventStore;
pub use frame::{AudioCodec, AudioFrame, MediaFrame, VideoCodec, VideoFrame};
pub(crate) use health::{RecordingHealthRegistry, RecordingStreamHealthSnapshot};
pub use identity::RecordingStreamIdentity;
pub use media_objects::{EncodedEventKeyframe, EventKeyframeLookup};
pub use search::{
    DEFAULT_PREVIEW_AFTER_MS, DEFAULT_PREVIEW_BEFORE_MS, EncodedEventPreview,
    EventAdvancedSearchQuery, EventEmbedding, EventEmbeddingInput, EventEmbeddingProvenance,
    EventImageFilter, EventMetadataQuery, EventSearch, EventSearchField, EventSearchHit,
    EventSearchPage, EventSearchPlannerDiagnostics, EventSearchSemanticSummary, EventSearchTerm,
    EventSemanticRanking, EventSemanticSearchQuery, EventTextSearchQuery,
};
pub use segment::RecordingFrame;
