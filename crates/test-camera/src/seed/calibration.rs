//! Seeds two owned AVC/AAC recordings on one clock for playback qualification.

use super::*;
use anyhow::ensure;
use keeppeek::storage::{
    CatalogFragment, CatalogKeyframe, CatalogRecording, RecordingCatalogHandle,
    RecordingStreamIdentity, StorageConfig,
    catalog::locations::{Kind, Object},
    volumes::VolumeRole,
};
use std::{
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

mod source;

struct Fragment {
    info: mp4::Mp4FragmentInfo,
    start_ms: i64,
    end_ms: i64,
}

struct Prepared {
    bytes: Vec<u8>,
    initialization_len: u64,
    fragments: Vec<Fragment>,
}

struct Owner {
    catalog: RecordingCatalog,
    storage: Option<StorageConfig>,
    recordings: PathBuf,
}

pub(super) fn seed(options: &RecordingSeedOptions) -> anyhow::Result<()> {
    ensure!(
        options.age >= Duration::from_secs(5),
        "calibration age must be at least five seconds"
    );
    let periods = source::render(&options.source)?;
    let origin = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?
        .checked_sub(i64::try_from(options.age.as_millis())?)
        .context("calibration age exceeds UTC clock")?;
    let identity = RecordingStreamIdentity::legacy(&options.stream_id);
    let owner = Owner::open(options)?;
    let mut manifest = Vec::new();
    for period in &periods {
        manifest.push(owner.publish(period, &identity, origin)?);
    }
    let start = manifest[0]["start_ms"]
        .as_i64()
        .context("calibration start is missing")?;
    let end = manifest[1]["end_ms"]
        .as_i64()
        .context("calibration end is missing")?;
    let output = serde_json::json!({
        "source_id": identity.source_id, "stream_id": identity.stream_id,
        "recording_stream_id": options.stream_id, "start_ms": start, "end_ms": end,
        "tone_lead_ms": 200, "periods": manifest,
    });
    drop(owner.storage);
    owner.catalog.shutdown();
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

impl Owner {
    fn open(options: &RecordingSeedOptions) -> anyhow::Result<Self> {
        if let Some(configuration) = &options.configuration {
            let mut storage = StorageConfig::from_toml(configuration);
            let binding = storage
                .metadata
                .as_ref()
                .context("calibration metadata owner is missing")?;
            let catalog = RecordingCatalog::open_managed(
                &storage.recording_catalog_path,
                &binding.authority(),
                &binding.root_identity(),
            )?;
            storage.initialize_named_volumes(catalog.handle())?;
            return Ok(Self {
                catalog,
                storage: Some(storage),
                recordings: PathBuf::new(),
            });
        }
        std::fs::create_dir_all(&options.recordings)?;
        Ok(Self {
            catalog: RecordingCatalog::open(&options.catalog)?,
            storage: None,
            recordings: options.recordings.clone(),
        })
    }

    fn publish(
        &self,
        period: &Prepared,
        identity: &RecordingStreamIdentity,
        origin: i64,
    ) -> anyhow::Result<serde_json::Value> {
        let id = uuid::Uuid::new_v4().to_string();
        if let Some(storage) = &self.storage {
            let reservation = storage
                .volume_runtime
                .as_ref()
                .context("calibration volumes are missing")?
                .reserve(
                    VolumeRole::Active,
                    &identity.source_id,
                    &[],
                    Object {
                        kind: Kind::Recording,
                        id: id.clone(),
                    },
                    u64::try_from(period.bytes.len())?,
                )?
                .context("calibration placement is unavailable")?;
            let path = reservation.path().to_path_buf();
            let mut file = reservation.open()?;
            file.write_all(&period.bytes)?;
            index(&self.catalog.handle(), period, identity, origin, &id, &path)?;
            let evidence = file.evidence()?;
            file.finalize(evidence)?;
        } else {
            let path = self.recordings.join(format!("{id}.mp4"));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(&period.bytes)?;
            file.sync_all()?;
            index(&self.catalog.handle(), period, identity, origin, &id, &path)?;
            self.catalog
                .handle()
                .update_recording_path(&id, &path, true)?;
        }
        Ok(
            serde_json::json!({ "recording_id": id, "fragments": period.fragments.len(),
            "start_ms": origin + period.fragments[0].start_ms,
            "end_ms": origin + period.fragments.last().context("calibration period is empty")?.end_ms }),
        )
    }
}

fn index(
    catalog: &RecordingCatalogHandle,
    period: &Prepared,
    identity: &RecordingStreamIdentity,
    origin: i64,
    id: &str,
    path: &Path,
) -> anyhow::Result<()> {
    // Keep one clock origin in both files. The second initialization retains nonzero decode times.
    catalog.upsert_recording(CatalogRecording {
        id: id.into(),
        stream_id: identity.storage_key.clone(),
        source_id: Some(identity.source_id.clone()),
        logical_stream_id: Some(identity.stream_id.clone()),
        started_at_ms: origin,
        ended_at_ms: Some(
            origin
                + period
                    .fragments
                    .last()
                    .context("empty calibration period")?
                    .end_ms,
        ),
        path: path
            .to_str()
            .context("calibration path is not UTF-8")?
            .into(),
        init_offset: 0,
        init_len: period.initialization_len,
        finalized: false,
    })?;
    for fragment in &period.fragments {
        let keyframe = fragment
            .info
            .video_keyframe
            .context("calibration GOP has no keyframe")?;
        catalog.insert_fragment_with_keyframe(
            CatalogFragment {
                recording_id: id.into(),
                sequence: u64::from(fragment.info.sequence_number),
                start_ms: origin + fragment.start_ms,
                duration_ms: u64::try_from(fragment.end_ms - fragment.start_ms)?,
                byte_offset: fragment.info.range.offset,
                byte_len: fragment.info.range.size,
                random_access: true,
            },
            CatalogKeyframe {
                recording_id: id.into(),
                fragment_sequence: u64::from(fragment.info.sequence_number),
                byte_offset: keyframe.offset,
                byte_len: u64::from(keyframe.size),
            },
        )?;
    }
    Ok(())
}
