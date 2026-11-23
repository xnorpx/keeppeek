//! Encodes stored fragments while retaining identifiers across cursor refills.

use super::*;
use crate::storage::{
    archive::{VerifiedReader, target::Work},
    catalog::archive_playback::ArchiveRead,
};

struct ArchiveSource<'a> {
    reader: VerifiedReader<'a>,
    init_offset: u64,
    initialization: Vec<u8>,
}

impl<'a> ArchiveSource<'a> {
    fn open(
        fragment: &CatalogMediaFragment,
        descriptor: &ArchiveRead,
        work: Work<'a>,
    ) -> anyhow::Result<Self> {
        let mut reader = VerifiedReader::new(descriptor.clone(), work)?;
        let initialization = reader.read_range(fragment.init_offset, fragment.init_len)?;
        Ok(Self {
            reader,
            init_offset: fragment.init_offset,
            initialization,
        })
    }

    fn matches(&self, fragment: &CatalogMediaFragment, descriptor: &ArchiveRead) -> bool {
        self.reader.descriptor() == descriptor
            && self.init_offset == fragment.init_offset
            && self.initialization.len() as u64 == fragment.init_len
    }
}

struct Encoder<'a> {
    id: &'a str,
    generation: u64,
    target: DataChannelTarget,
    identifiers: StoredMediaIdentifiers,
    period: Option<StoredMediaPeriodKey>,
    archive_source: Option<ArchiveSource<'a>>,
    archive_deadline: Instant,
    content_type: Option<String>,
    messages: Vec<OutboundDataMessage>,
}

pub(super) fn encode(
    id: &str,
    generation: u64,
    time_ms: i64,
    target: DataChannelTarget,
    fragments: Vec<CatalogMediaFragment>,
    identifiers: StoredMediaIdentifiers,
) -> Result<StoredMediaBatch, ControlCommandError> {
    let archive = fragments.iter().any(|fragment| fragment.archive.is_some());
    let delivered = fragments.iter().fold(time_ms, |end, fragment| {
        end.max(
            fragment
                .start_ms
                .saturating_add(i64::try_from(fragment.duration_ms).unwrap_or(i64::MAX)),
        )
    });
    let mut encoder = Encoder {
        id,
        generation,
        target,
        identifiers,
        period: None,
        archive_source: None,
        archive_deadline: Instant::now() + Duration::from_secs(30),
        content_type: None,
        messages: Vec::new(),
    };
    for fragment in fragments {
        encoder.push(fragment)?;
    }
    Ok(StoredMediaBatch {
        archive,
        audio_timing: None,
        identifiers: encoder.identifiers,
        content_type: encoder
            .content_type
            .unwrap_or_else(|| "video/mp4".to_owned()),
        fragment_time_ms: time_ms,
        delivered_through_ms: delivered,
        messages: encoder.messages,
    })
}

impl Encoder<'_> {
    fn push(&mut self, fragment: CatalogMediaFragment) -> Result<(), ControlCommandError> {
        let period = self.read_period(&fragment)?;
        let key = StoredMediaPeriodKey {
            recording_id: fragment.recording_id,
            sample_descriptions: period.sample_descriptions,
        };
        if self.period.as_ref() != Some(&key) {
            self.identifiers.initialization_id = increment(self.identifiers.initialization_id)?;
            self.content_type
                .get_or_insert_with(|| period.content_type.clone());
            append_initialization_messages(
                &mut self.messages,
                self.id,
                self.generation,
                self.identifiers.initialization_id,
                &period.content_type,
                &period.initialization,
            )?;
            self.period = Some(key);
        }
        self.identifiers.sequence = increment(self.identifiers.sequence)?;
        append_fragment_messages(
            &mut self.messages,
            self.id,
            self.generation,
            self.identifiers.initialization_id,
            self.identifiers.sequence,
            fragment.start_ms,
            fragment.duration_ms,
            self.target,
            &period.fragment,
        )
    }

    fn read_period(
        &mut self,
        fragment: &CatalogMediaFragment,
    ) -> Result<StoredMediaPeriod, ControlCommandError> {
        let Some(archive) = &fragment.archive else {
            self.archive_source = None;
            let path = Path::new(&fragment.path);
            let initialization =
                read_stored_range(path, None, fragment.init_offset, fragment.init_len)?;
            let payload = read_stored_range(path, None, fragment.byte_offset, fragment.byte_len)?;
            return stored_media_period(&initialization, &payload);
        };
        stored_read::validate_length(fragment.init_len)?;
        stored_read::validate_length(fragment.byte_len)?;
        // Discard the previous initialization and chunk before opening another object.
        self.archive_source
            .take_if(|source| !source.matches(fragment, archive));
        let source = match &mut self.archive_source {
            Some(source) => source,
            None => {
                let work = Work {
                    deadline: self.archive_deadline,
                    cancelled: &|| false,
                };
                self.archive_source.insert(
                    ArchiveSource::open(fragment, archive, work)
                        .map_err(stored_read::archive_error)?,
                )
            }
        };
        let payload = source
            .reader
            .read_range(fragment.byte_offset, fragment.byte_len)
            .map_err(stored_read::archive_error)?;
        stored_media_period(&source.initialization, &payload)
    }
}

fn increment(value: u64) -> Result<u64, ControlCommandError> {
    value.checked_add(1).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "stored media message identifier overflowed",
        )
    })
}

#[cfg(test)]
mod tests;
