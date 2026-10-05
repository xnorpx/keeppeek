//! Reuses retirement receipts for proven empty or initialization-only recordings.

use super::{Action, Manager, Mode, OwnedFile, Pending, Plan, Publication, Request, container};
use std::time::{Duration, Instant};

pub(super) fn plan(file: &mut OwnedFile, pending: &Pending) -> anyhow::Result<Plan> {
    anyhow::ensure!(
        pending.fragments.is_empty(),
        "indexed recording cannot be abandoned"
    );
    let before = file.file_mut().metadata()?;
    let bytes = before.len();
    anyhow::ensure!(
        bytes == 0 || (pending.init_offset == 0 && bytes == pending.init_bytes),
        "unindexed recording bytes require inspection"
    );
    if bytes != 0 {
        container::inspect_initialization(
            file.file_mut(),
            bytes,
            Instant::now() + Duration::from_secs(2),
        )?;
    }
    let (actual, file_identity, digest) = file.evidence()?;
    let after = file.file_mut().metadata()?;
    anyhow::ensure!(
        actual == bytes && before.modified()? == after.modified()?,
        "recording changed during abandonment inspection"
    );
    Ok(Plan {
        mode: Mode::Abandon,
        original_bytes: bytes,
        last_sequence: 0,
        evidence: Publication {
            operation: pending.owned.operation.clone(),
            bytes,
            file_identity,
            digest,
        },
    })
}

impl Manager {
    pub(super) fn finish_abandoned_recording(
        &self,
        pending: &Pending,
        plan: &Plan,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            plan.mode == Mode::Abandon,
            "recording abandonment plan required"
        );
        let root = self.recording_root(pending)?;
        let evidence = &plan.evidence;
        if !pending.complete {
            root.retire_owned(
                &pending.owned.relative_key,
                &evidence.file_identity,
                evidence.bytes,
                evidence.digest,
                &evidence.operation,
            )?;
            self.inner
                .catalog
                .volume_location(Request::RecordingRecovery(Action::Complete(
                    evidence.clone(),
                )))?;
        }
        root.acknowledge_retirement(
            &pending.owned.relative_key,
            &evidence.file_identity,
            evidence.bytes,
            evidence.digest,
            &evidence.operation,
        )?;
        self.inner
            .catalog
            .volume_location(Request::RecordingRecovery(Action::Acknowledge(
                evidence.operation.clone(),
            )))?;
        Ok(true)
    }
}
