//! Encodes stored fragments while retaining identifiers across cursor refills.

use super::*;

struct Encoder<'a> {
    id: &'a str,
    generation: u64,
    target: DataChannelTarget,
    identifiers: StoredMediaIdentifiers,
    period: Option<StoredMediaPeriodKey>,
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
        let initialization = read_stored_range(
            Path::new(&fragment.path),
            fragment.archive.as_ref(),
            fragment.init_offset,
            fragment.init_len,
        )?;
        let payload = read_stored_range(
            Path::new(&fragment.path),
            fragment.archive.as_ref(),
            fragment.byte_offset,
            fragment.byte_len,
        )?;
        let period = stored_media_period(&initialization, &payload)?;
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
