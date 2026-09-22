use super::{MAX_ACTOR_BYTES, MAX_REASON_BYTES, Snapshot, validate_ids, validate_text};
use crate::storage::{
    catalog::{BUSY_TIMEOUT, Command, RecordingCatalogHandle, to_u64},
    long_term::inspection::{Archive, IDENTITY_BYTES_MAX, Identity, PATH_BYTES_MAX},
};
use std::{sync::mpsc, time::Instant};

/// A coherent catalog snapshot plus a point-in-time file identity and size observation.
#[derive(Debug, Clone)]
pub struct Inspection {
    pub hold: Option<Snapshot>,
    pub protected: bool,
    pub independently_protected: bool,
    pub eligible: bool,
    pub bytes: u64,
    pub media_available: bool,
}

pub(in crate::storage::catalog) struct Inputs {
    state: Inspection,
    path: String,
    identity: Option<Identity>,
}

impl RecordingCatalogHandle {
    /// Does not read media contents or guarantee continuing filesystem availability.
    /// Missing or inaccessible media never hides an existing preservation marker.
    pub fn inspect_recording_hold(
        &self,
        recording_id: &str,
        hold_id: &str,
        archive: Option<&Archive>,
    ) -> anyhow::Result<Inspection> {
        validate_ids(recording_id, hold_id)?;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::InspectHold {
                recording_id: recording_id.to_owned(),
                hold_id: hold_id.to_owned(),
                deadline,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("recording catalog is unavailable or busy"))?;
        let mut inputs = response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| anyhow::anyhow!("recording inspection is unavailable; reload state"))??;
        inputs.state.media_available = archive.is_some_and(|archive| {
            archive
                .inspect_until(&inputs.path, inputs.state.bytes, deadline)
                .is_ok_and(|file| Some(file.identity()) == inputs.identity)
        });
        Ok(inputs.state)
    }
}

pub(in crate::storage::catalog) async fn read(
    connection: &turso::Connection,
    recording_id: &str,
    hold_id: &str,
    deadline: Instant,
) -> anyhow::Result<Inputs> {
    anyhow::ensure!(Instant::now() < deadline, "recording inspection expired");
    let mut rows = connection.query(
        "SELECT h.revision, h.active, h.actor, h.reason, r.protected,
          EXISTS(SELECT 1 FROM recording_holds WHERE recording_id = r.id AND active = 1 AND hold_id != ?2)
          OR CASE WHEN EXISTS(SELECT 1 FROM recording_holds WHERE recording_id = r.id AND active = 1)
            THEN COALESCE((SELECT protected FROM recording_hold_baselines WHERE recording_id = r.id), 0)
            ELSE r.protected END,
          r.finalized = 1 AND r.ended_at_ms IS NOT NULL AND r.cleanup_pending = 0
            AND NOT EXISTS(SELECT 1 FROM recording_maintenance_claims WHERE recording_id = r.id AND active = 1),
          r.file_bytes,
          CASE WHEN length(CAST(r.path AS BLOB)) <= ?3 THEN r.path ELSE '' END,
          CASE WHEN length(CAST(r.file_identity AS BLOB)) <= ?4 THEN r.file_identity ELSE NULL END
         FROM recording_files r LEFT JOIN recording_holds h ON h.recording_id = r.id AND h.hold_id = ?2
         WHERE r.id = ?1",
        turso::params![recording_id, hold_id, PATH_BYTES_MAX as i64, IDENTITY_BYTES_MAX as i64],
    ).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("recording is unavailable"))?;
    let hold = row
        .get::<Option<i64>>(0)?
        .map(|revision| {
            let state = Snapshot {
                revision: to_u64(revision, "hold revision")?,
                active: row.get::<i64>(1)? != 0,
                actor: row.get(2)?,
                reason: row.get(3)?,
            };
            anyhow::ensure!(state.revision > 0, "invalid hold revision");
            validate_text(&state.actor, MAX_ACTOR_BYTES)?;
            validate_text(&state.reason, MAX_REASON_BYTES)?;
            Ok(state)
        })
        .transpose()?;
    anyhow::ensure!(Instant::now() < deadline, "recording inspection expired");
    Ok(Inputs {
        state: Inspection {
            hold,
            protected: row.get::<i64>(4)? != 0,
            independently_protected: row.get::<i64>(5)? != 0,
            eligible: row.get::<i64>(6)? != 0,
            bytes: to_u64(row.get(7)?, "recording bytes")?,
            media_available: false,
        },
        path: row.get(8)?,
        identity: row
            .get::<Option<String>>(9)?
            .as_deref()
            .and_then(Identity::parse),
    })
}
