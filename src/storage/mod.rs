pub mod adts;
#[doc(hidden)]
pub mod benchmark;
pub mod catalog;
pub mod demand;
pub mod engine;
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
mod recording_policy;
pub(crate) mod safety;
pub mod search;
pub mod segment;
pub mod short_term;

pub use catalog::{
    CatalogEventKeyframeLink, CatalogFragment, CatalogKeyframe, CatalogMediaFragment,
    CatalogMediaObjectLocation, CatalogRecording, EventKeyframeLocation, RecordingCatalog,
    RecordingCatalogHandle,
};
pub use demand::{RecordingDemand, RecordingDemandGuard};
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
