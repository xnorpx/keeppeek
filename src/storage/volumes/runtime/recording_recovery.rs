//! Seals the indexed, playable prefix of an interrupted named recording.

use super::{Manager, Publication, Reply, Request};
use crate::storage::{
    catalog::locations::recording_recovery::{Action, Mode, Pending, Plan},
    long_term::inspection::container,
    volumes::root::{OwnedFile, Root},
};
use std::time::{Duration, Instant};
mod abandonment;

impl Manager {
    pub(super) fn recover_pending_recording(&self, operation: &str) -> anyhow::Result<bool> {
        if self.pending_recording(operation)?.is_none() {
            return Ok(false);
        }
        let _writer = self.inner.catalog.claim_volume_move(operation)?;
        let Some(pending) = self.pending_recording(operation)? else {
            return Ok(true);
        };
        if pending.owned.file_identity.is_none() {
            return self.release_unopened_recording(pending);
        }
        let plan = if let Some(plan) = &pending.plan {
            plan.clone()
        } else {
            self.begin_recording_recovery(&pending)?
        };
        if plan.mode == Mode::Abandon {
            return self.finish_abandoned_recording(&pending, &plan);
        }
        let mut file = self.open_interrupted_recording(&pending)?;
        file.retain_verified_prefix(
            plan.evidence.bytes,
            plan.original_bytes,
            plan.evidence.digest,
        )?;
        let (bytes, file_identity, digest) = file.evidence()?;
        let evidence = Publication {
            operation: operation.into(),
            bytes,
            file_identity,
            digest,
        };
        anyhow::ensure!(
            evidence == plan.evidence,
            "recovered recording evidence changed"
        );
        self.inner
            .catalog
            .volume_location(Request::RecordingRecovery(Action::Complete(evidence)))?;
        self.inner
            .rescan_requested
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(true)
    }

    pub(super) fn begin_recording_recovery(&self, pending: &Pending) -> anyhow::Result<Plan> {
        let mut file = self.open_interrupted_recording(pending)?;
        let plan = if pending.fragments.is_empty() {
            abandonment::plan(&mut file, pending)?
        } else {
            recovery_plan(&mut file, pending)?
        };
        self.inner
            .catalog
            .volume_location(Request::RecordingRecovery(Action::Begin(
                Box::new(pending.clone()),
                plan.clone(),
            )))?;
        Ok(plan)
    }

    fn release_unopened_recording(&self, pending: Box<Pending>) -> anyhow::Result<bool> {
        self.recording_root(&pending)?
            .confirm_absent(&[&pending.owned.relative_key])?;
        self.inner
            .catalog
            .volume_location(Request::RecordingRecovery(Action::ReleaseUnopened(pending)))?;
        Ok(true)
    }

    fn recording_root(&self, pending: &Pending) -> anyhow::Result<&Root> {
        anyhow::ensure!(
            pending.owned.generation == 1,
            "recording volume generation changed"
        );
        let index = self
            .inner
            .configuration
            .volumes
            .iter()
            .position(|volume| volume.id.as_str() == pending.owned.volume)
            .ok_or_else(|| anyhow::anyhow!("recording volume is not configured"))?;
        self.inner.writable_root(index)
    }

    fn open_interrupted_recording(&self, pending: &Pending) -> anyhow::Result<OwnedFile> {
        let root = self.recording_root(pending)?;
        let identity = pending
            .owned
            .file_identity
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("recording identity is unavailable"))?;
        let minimum = pending
            .plan
            .as_ref()
            .map_or(pending.owned.materialized_bytes, |plan| plan.evidence.bytes);
        let maximum = pending
            .plan
            .as_ref()
            .map_or(pending.owned.bytes, |plan| plan.original_bytes);
        root.open_owned_writable(&pending.owned.relative_key, identity, minimum, maximum)
    }

    fn pending_recording(&self, operation: &str) -> anyhow::Result<Option<Box<Pending>>> {
        let Reply::PendingRecording(pending) = self
            .inner
            .catalog
            .volume_location(Request::RecordingRecovery(Action::Load(operation.into())))?
        else {
            anyhow::bail!("invalid pending recording reply");
        };
        Ok(pending)
    }
}

pub(super) fn recovery_plan(file: &mut OwnedFile, pending: &Pending) -> anyhow::Result<Plan> {
    let before = file.file_mut().metadata()?;
    let original_bytes = before.len();
    let (endpoint, retained) = prefix_endpoint(pending, original_bytes)?;
    let parsed = container::inspect(
        file.file_mut(),
        endpoint,
        Instant::now() + Duration::from_secs(2),
    )?;
    validate_index(pending, &parsed, retained)?;
    let digest = file.prefix_digest(endpoint)?;
    let after = file.file_mut().metadata()?;
    anyhow::ensure!(
        before.len() == after.len() && before.modified()? == after.modified()?,
        "recording changed during recovery inspection"
    );
    Ok(Plan {
        mode: Mode::Seal,
        original_bytes,
        last_sequence: pending.fragments[retained - 1].sequence,
        evidence: Publication {
            operation: pending.owned.operation.clone(),
            bytes: endpoint,
            file_identity: pending
                .owned
                .file_identity
                .clone()
                .ok_or_else(|| anyhow::anyhow!("recording identity missing"))?,
            digest,
        },
    })
}

fn prefix_endpoint(pending: &Pending, original_bytes: u64) -> anyhow::Result<(u64, usize)> {
    let mut endpoint = pending
        .init_offset
        .checked_add(pending.init_bytes)
        .ok_or_else(|| anyhow::anyhow!("initialization range overflow"))?;
    let mut retained = 0;
    for fragment in &pending.fragments {
        anyhow::ensure!(
            fragment.offset == endpoint && fragment.bytes > 0,
            "recording fragments are not a contiguous prefix"
        );
        let end = fragment
            .offset
            .checked_add(fragment.bytes)
            .ok_or_else(|| anyhow::anyhow!("fragment range overflow"))?;
        if end > original_bytes {
            break;
        }
        anyhow::ensure!(
            fragment.key_offset.is_some() && fragment.key_bytes.is_some(),
            "complete recording fragment lacks keyframe metadata"
        );
        endpoint = end;
        retained += 1;
    }
    anyhow::ensure!(retained > 0, "recording has no complete indexed fragment");
    Ok((endpoint, retained))
}

fn validate_index(
    pending: &Pending,
    parsed: &container::Index,
    retained: usize,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        parsed.initialization.offset == pending.init_offset
            && parsed.initialization.size == pending.init_bytes
            && parsed.fragments.len() == retained,
        "recording initialization or fragment count changed"
    );
    for (expected, actual) in pending.fragments[..retained].iter().zip(&parsed.fragments) {
        anyhow::ensure!(
            actual.range.offset == expected.offset
                && actual.range.size == expected.bytes
                && Some(actual.first_sample.location.offset) == expected.key_offset
                && Some(u64::from(actual.first_sample.location.size)) == expected.key_bytes
                && u64::from(actual.first_sample.sequence_number) == expected.sequence
                && pending
                    .started_at_ms
                    .checked_add(i64::try_from(actual.start_ms)?)
                    == Some(expected.start_ms)
                && actual.duration_ms == expected.duration_ms
                && expected.random_access,
            "recording fragment or keyframe range changed"
        );
    }
    Ok(())
}
