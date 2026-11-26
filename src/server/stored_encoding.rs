//! Encodes stored fragments while retaining identifiers across cursor refills.

use super::*;
use crate::storage::{
    archive::{VerifiedReader, target::Work},
    catalog::archive_playback::ArchiveRead,
};

struct SingleDescriptionInit {
    sample_descriptions: Vec<u32>,
    initialization: Vec<u8>,
    content_type: String,
}

struct LocalSource {
    path: String,
    init_offset: u64,
    init_len: u64,
    file: File,
    file_len: u64,
    initialization: Vec<u8>,
    single_description: Option<SingleDescriptionInit>,
}

impl LocalSource {
    fn open(fragment: &CatalogMediaFragment) -> Result<Self, ControlCommandError> {
        stored_read::validate_length(fragment.init_len)?;
        let mut file = File::open(&fragment.path).map_err(|error| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                format!("unable to open indexed recording: {error}"),
            )
        })?;
        let file_len = file
            .metadata()
            .map_err(|error| {
                ControlCommandError::new(
                    proto::ErrorCode::Internal,
                    500,
                    format!("unable to inspect indexed recording: {error}"),
                )
            })?
            .len();
        let initialization =
            read_local_range(&mut file, file_len, fragment.init_offset, fragment.init_len)?;
        let single_description = parse_single_description_init(&initialization)?;
        Ok(Self {
            path: fragment.path.clone(),
            init_offset: fragment.init_offset,
            init_len: fragment.init_len,
            file,
            file_len,
            initialization,
            single_description,
        })
    }

    fn matches(&self, fragment: &CatalogMediaFragment) -> bool {
        self.path == fragment.path
            && self.init_offset == fragment.init_offset
            && self.init_len == fragment.init_len
    }

    fn read_fragment(
        &mut self,
        fragment: &CatalogMediaFragment,
    ) -> Result<Vec<u8>, ControlCommandError> {
        stored_read::validate_length(fragment.byte_len)?;
        read_local_range(
            &mut self.file,
            self.file_len,
            fragment.byte_offset,
            fragment.byte_len,
        )
    }
}

struct ArchiveSource<'a> {
    reader: VerifiedReader<'a>,
    init_offset: u64,
    initialization: Vec<u8>,
    single_description: Option<SingleDescriptionInit>,
}

impl<'a> ArchiveSource<'a> {
    fn open(
        fragment: &CatalogMediaFragment,
        descriptor: &ArchiveRead,
        work: Work<'a>,
    ) -> Result<Self, ControlCommandError> {
        let mut reader =
            VerifiedReader::new(descriptor.clone(), work).map_err(stored_read::archive_error)?;
        let initialization = reader
            .read_range(fragment.init_offset, fragment.init_len)
            .map_err(stored_read::archive_error)?;
        let single_description = parse_single_description_init(&initialization)?;
        Ok(Self {
            reader,
            init_offset: fragment.init_offset,
            initialization,
            single_description,
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
    local_source: Option<LocalSource>,
    archive_source: Option<ArchiveSource<'a>>,
    archive_deadline: Option<Instant>,
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
        local_source: None,
        archive_source: None,
        archive_deadline: None,
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
            self.local_source
                .take_if(|source| !source.matches(fragment));
            let source = match &mut self.local_source {
                Some(source) => source,
                None => self.local_source.insert(LocalSource::open(fragment)?),
            };
            let payload = source.read_fragment(fragment)?;
            if let Some(single) = &source.single_description {
                let normalized = mp4::normalize_fragment_sample_description_indices(&payload)
                    .map_err(|error| stored_mp4_period_error("normalize fragment", error))?;
                return Ok(StoredMediaPeriod {
                    sample_descriptions: single.sample_descriptions.clone(),
                    initialization: single.initialization.clone(),
                    fragment: normalized,
                    content_type: single.content_type.clone(),
                });
            }
            return stored_media_period(&source.initialization, &payload);
        };
        self.local_source = None;
        stored_read::validate_length(fragment.init_len)?;
        stored_read::validate_length(fragment.byte_len)?;
        // Discard the previous initialization and chunk before opening another object.
        self.archive_source
            .take_if(|source| !source.matches(fragment, archive));
        let source = match &mut self.archive_source {
            Some(source) => source,
            None => {
                let work = Work {
                    deadline: *self
                        .archive_deadline
                        .get_or_insert_with(|| Instant::now() + Duration::from_secs(30)),
                    cancelled: &|| false,
                };
                self.archive_source
                    .insert(ArchiveSource::open(fragment, archive, work)?)
            }
        };
        let payload = source
            .reader
            .read_range(fragment.byte_offset, fragment.byte_len)
            .map_err(stored_read::archive_error)?;
        if let Some(single) = &source.single_description {
            let normalized = mp4::normalize_fragment_sample_description_indices(&payload)
                .map_err(|error| stored_mp4_period_error("normalize fragment", error))?;
            return Ok(StoredMediaPeriod {
                sample_descriptions: single.sample_descriptions.clone(),
                initialization: single.initialization.clone(),
                fragment: normalized,
                content_type: single.content_type.clone(),
            });
        }
        stored_media_period(&source.initialization, &payload)
    }
}

fn read_local_range(
    file: &mut File,
    file_len: u64,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, ControlCommandError> {
    if offset.checked_add(length).is_none_or(|end| end > file_len) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "stored media byte range exceeds its indexed recording",
        ));
    }
    file.seek(SeekFrom::Start(offset)).map_err(|error| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            format!("unable to seek indexed recording: {error}"),
        )
    })?;
    let capacity = usize::try_from(length).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "stored media byte range does not fit memory",
        )
    })?;
    let mut payload = vec![0; capacity];
    file.read_exact(&mut payload).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::UnexpectedEof {
            "indexed recording byte range was truncated".to_owned()
        } else {
            format!("unable to read indexed recording: {error}")
        };
        ControlCommandError::new(proto::ErrorCode::Internal, 500, message)
    })?;
    Ok(payload)
}

fn parse_single_description_init(
    initialization: &[u8],
) -> Result<Option<SingleDescriptionInit>, ControlCommandError> {
    let reader = mp4::Mp4Reader::read_header(
        Cursor::new(initialization),
        initialization.len().try_into().unwrap_or(u64::MAX),
    )
    .map_err(|error| stored_mp4_period_error("parse initialization", error))?;
    let mut tracks = reader.tracks().iter().collect::<Vec<_>>();
    tracks.sort_unstable_by_key(|(track_id, _)| **track_id);
    if tracks
        .iter()
        .any(|(_, track)| track.sample_description_count() > 1)
    {
        return Ok(None);
    }
    let mut sample_descriptions = Vec::with_capacity(tracks.len());
    let mut track_configs = Vec::with_capacity(tracks.len());
    for (_, track) in tracks {
        sample_descriptions.push(1);
        track_configs.push(mp4::FragmentedTrackConfig {
            track_type: track
                .track_type()
                .map_err(|error| stored_mp4_period_error("read track type", error))?,
            timescale: track.timescale(),
            language: track.language().to_owned(),
            sample_descriptions: vec![
                track
                    .media_config_for_description(1)
                    .map_err(|error| stored_mp4_period_error("read sample description", error))?,
            ],
        });
    }
    let config = mp4::Mp4Config {
        major_brand: *reader.major_brand(),
        minor_version: reader.minor_version(),
        compatible_brands: reader.compatible_brands().to_vec(),
        timescale: reader.timescale(),
    };
    let writer = mp4::FragmentedMp4Writer::write_start_with_sample_descriptions(
        Cursor::new(Vec::new()),
        &config,
        &track_configs,
    )
    .map_err(|error| stored_mp4_period_error("write initialization", error))?;
    let range = writer.initialization();
    let bytes = writer.into_writer().into_inner();
    let normalized_init =
        bytes[range.offset as usize..(range.offset + range.size) as usize].to_vec();
    let content_type = fragmented_mp4_content_type(&normalized_init)?;
    Ok(Some(SingleDescriptionInit {
        sample_descriptions,
        initialization: normalized_init,
        content_type,
    }))
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
