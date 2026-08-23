/// Stable source/stream identity plus the storage-layout key used for recording files.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecordingStreamIdentity {
    pub source_id: String,
    pub stream_id: String,
    pub storage_key: String,
}

impl RecordingStreamIdentity {
    pub fn new(
        source_id: impl Into<String>,
        stream_id: impl Into<String>,
        storage_label: &str,
    ) -> Self {
        let stream_id = stream_id.into();
        Self {
            source_id: source_id.into(),
            storage_key: format!("{storage_label}/{stream_id}"),
            stream_id,
        }
    }

    pub fn legacy(storage_key: impl Into<String>) -> Self {
        let storage_key = storage_key.into();
        let (source_id, stream_id) = storage_key.rsplit_once('/').map_or_else(
            || (storage_key.clone(), "main".to_owned()),
            |(source_id, stream_id)| (source_id.to_owned(), stream_id.to_owned()),
        );
        Self {
            source_id,
            stream_id,
            storage_key,
        }
    }
}
