//! Exact indexed reads of encoded keyframes stored inside MP4 files.

use crate::storage::{EventKeyframeLocation, RecordingCatalogHandle};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Debug, Clone, PartialEq, Eq)]
/// One resolved keyframe and its owned encoded bytes.
pub struct EncodedEventKeyframe {
    pub location: EventKeyframeLocation,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
/// Resolves event links and reads exact encoded keyframe byte ranges.
pub struct EventKeyframeLookup {
    catalog: RecordingCatalogHandle,
}

impl EventKeyframeLookup {
    /// Creates a lookup facade over the catalog handle.
    pub const fn new(catalog: RecordingCatalogHandle) -> Self {
        Self { catalog }
    }

    /// Resolves an event and logical stream without reading its MP4 bytes.
    pub fn resolve(
        &self,
        event_id: &str,
        stream_id: &str,
    ) -> anyhow::Result<Option<EventKeyframeLocation>> {
        self.catalog.resolve_event_keyframe(event_id, stream_id)
    }

    /// Resolves and reads one event keyframe.
    pub fn read(
        &self,
        event_id: &str,
        stream_id: &str,
    ) -> anyhow::Result<Option<EncodedEventKeyframe>> {
        let Some(location) = self.resolve(event_id, stream_id)? else {
            return Ok(None);
        };
        self.read_location(location).map(Some)
    }

    /// Reads an already resolved keyframe location.
    pub fn read_location(
        &self,
        location: EventKeyframeLocation,
    ) -> anyhow::Result<EncodedEventKeyframe> {
        let end = location
            .byte_offset
            .checked_add(location.byte_len)
            .ok_or_else(|| anyhow::anyhow!("keyframe byte range overflows"))?;
        let mut file = File::open(Path::new(&location.path))?;
        if end > file.metadata()?.len() {
            anyhow::bail!("keyframe byte range exceeds the recording file");
        }
        let length = usize::try_from(location.byte_len)
            .map_err(|_| anyhow::anyhow!("keyframe is too large to read on this platform"))?;
        let mut bytes = vec![0; length];
        file.seek(SeekFrom::Start(location.byte_offset))?;
        file.read_exact(&mut bytes)?;
        Ok(EncodedEventKeyframe { location, bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        CatalogFragment, CatalogKeyframe, CatalogRecording, RecordingCatalog,
        metadata::{EventSource, TimelineEvent},
    };

    #[test]
    fn lookup_resolves_and_reads_exact_keyframe_bytes() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-event-keyframe-lookup-{}",
            rand::random::<u64>()
        ));
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
                started_at_ms: 1_000,
                ended_at_ms: Some(3_000),
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
                    start_ms: 1_000,
                    duration_ms: 2_000,
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
        handle
            .insert_event(TimelineEvent {
                id: "event-1".to_owned(),
                revision: 1,
                camera_id: "front-door".to_owned(),
                stream: Some("main".to_owned()),
                source: EventSource::Camera,
                kind: "motion".to_owned(),
                start_time_ms: 1_500,
                end_time_ms: Some(1_700),
                confidence: None,
                bbox: None,
                bbox_attachment_id: None,
                zone: None,
                text: None,
                payload: None,
                attachments: Vec::new(),
                canonical_attachment_id: None,
                icon_key: "motion".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: None,
            })
            .unwrap();
        let lookup = EventKeyframeLookup::new(handle.clone());
        let keyframe = lookup.read("event-1", "main").unwrap().unwrap();
        assert_eq!(keyframe.bytes, b"encoded-keyframe");
        assert_eq!(keyframe.location.event_time_ms, 1_500);
        assert_eq!(keyframe.location.fragment_start_ms, 1_000);
        assert!(lookup.read("event-1", "sub").unwrap().is_none());

        drop(lookup);
        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
}
