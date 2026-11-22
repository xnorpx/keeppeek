pub mod adts;
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
const RECORDING_METADATA_BYTES_MAX: u64 = 8 * 1024 * 1024;
const RECORDING_BOX_COUNT_MAX: usize = 16_384;

const fn recording_metadata_rotation_due(bytes: u64, boxes: u64) -> bool {
    bytes >= RECORDING_METADATA_BYTES_MAX * 3 / 4 || boxes >= RECORDING_BOX_COUNT_MAX as u64 * 3 / 4
}

pub mod medium_term;
pub mod metadata;
pub mod nal;
pub mod playback;
mod pre_record;
mod recording_policy;
pub mod retention;
pub(crate) mod safety;
pub mod search;
pub mod segment;
pub mod short_term;
pub mod volumes;

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
    DEFAULT_PREVIEW_AFTER_MS, DEFAULT_PREVIEW_BEFORE_MS, EncodedEventPreview, EventEmbedding,
    EventImageFilter, EventMetadataQuery, EventSearch, EventSearchField, EventSearchHit,
    EventSearchPage, EventSearchTerm, EventSemanticSearchQuery, EventTextSearchQuery,
};
pub use segment::RecordingFrame;
