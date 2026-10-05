//! Transfers stopped metadata owners through the existing catalog authority fence.

use super::*;
use crate::storage::volumes::root::Root;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, Write};

#[derive(Clone, Copy)]
pub struct TransferCheck<'a> {
    pub authority: &'a Authority,
    pub destination: &'a Root,
    pub source: Option<&'a Root>,
    pub volume: &'a crate::storage::volumes::Volume,
    pub validate_history: fn(&[u8]) -> anyhow::Result<()>,
}

impl TransferCheck<'_> {
    fn owner(&self, connection: &turso::Connection, owner: &Record) -> anyhow::Result<()> {
        anyhow::ensure!(
            owner.authority.catalog_id == self.authority.catalog_id
                && owner.authority.generation.checked_add(1) == Some(self.authority.generation),
            "metadata handoff authority changed"
        );
        let binding =
            crate::storage::catalog::locations::Binding::metadata(self.volume, self.destination);
        pollster::block_on(crate::storage::catalog::locations::check_binding(
            connection, &binding,
        ))?;
        Ok(())
    }

    fn history(&self, connection: &turso::Connection, bytes: &[u8]) -> anyhow::Result<()> {
        (self.validate_history)(bytes)?;
        let required = crate::backup::database::snapshot_size_limit(connection)?
            .checked_add(u64::try_from(bytes.len())?)
            .ok_or_else(|| anyhow::anyhow!("metadata size overflow"))?;
        anyhow::ensure!(
            self.volume.capacity_bytes.is_none_or(|cap| required <= cap),
            "metadata exceeds the destination capacity limit"
        );
        let available = self.destination.capacity(0)?.available_bytes;
        let reserve = self
            .volume
            .minimum_free_bytes
            .max(self.volume.critical_free_bytes);
        anyhow::ensure!(
            available.saturating_sub(reserve) >= required,
            "metadata destination has insufficient free space"
        );
        Ok(())
    }

    fn roots(&self, leases: &[&Lease; 4]) -> anyhow::Result<()> {
        leases[1].require_root(self.destination)?;
        leases[3].require_root(self.destination)?;
        if let Some(root) = self.source {
            leases[0].require_root(root)?;
            leases[2].require_root(root)?;
        }
        Ok(())
    }
}

const HISTORY_BYTES_MAX: u64 = 8 * 1024 * 1024;
const HISTORY_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS recording_catalog_history_transfer (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    intent TEXT NOT NULL CHECK(length(intent)<=65536),
    destination_identity TEXT CHECK(length(destination_identity)<=128),
    complete INTEGER NOT NULL DEFAULT 0 CHECK(complete IN (0,1))
)";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    paths: [String; 4],
    locks: [String; 4],
    source_identity: String,
    bytes: u64,
    digest: [u8; 32],
    handoff: String,
}

struct Receipt {
    intent: Intent,
    destination_identity: Option<String>,
    complete: bool,
}

/// Requires a stopped source and existing parent directories; retains both source files.
#[cfg(test)]
pub fn transfer(
    source_path: &Path,
    destination_path: &Path,
    source_history_path: &Path,
    destination_history_path: &Path,
) -> anyhow::Result<()> {
    transfer_inner(
        source_path,
        destination_path,
        source_history_path,
        destination_history_path,
        None,
    )
}

pub fn transfer_checked(
    source_path: &Path,
    destination_path: &Path,
    source_history_path: &Path,
    destination_history_path: &Path,
    check: TransferCheck<'_>,
) -> anyhow::Result<()> {
    transfer_inner(
        source_path,
        destination_path,
        source_history_path,
        destination_history_path,
        Some(check),
    )
}

fn transfer_inner(
    source_path: &Path,
    destination_path: &Path,
    source_history_path: &Path,
    destination_history_path: &Path,
    expected: Option<TransferCheck<'_>>,
) -> anyhow::Result<()> {
    let mut source = Lease::acquire(source_path)?;
    anyhow::ensure!(source.file.is_some(), "source catalog is unavailable");
    let mut destination = Lease::acquire(destination_path)?;
    let history = Lease::acquire(source_history_path)?;
    let mut target_history = Lease::acquire(destination_history_path)?;
    distinct(&[&source, &destination, &history, &target_history])?;
    if let Some(expected) = expected {
        expected.roots(&[&source, &destination, &history, &target_history])?;
    }
    let connection = source.connect()?;
    let leases = [&source, &destination, &history, &target_history];
    pollster::block_on(connection.execute_batch(SCHEMA))?;
    if load(&connection)?.is_none() {
        anyhow::ensure!(expected.is_none(), "metadata source authority is missing");
        source.initialize(&connection)?;
    }
    let owner = required(&connection)?;
    if let Some(checked) = expected {
        checked.owner(&connection, &owner)?;
    }
    anyhow::ensure!(
        owner.identity == source.file_identity()?,
        "source catalog changed"
    );
    let receipt = prepare(&connection, &leases, &owner, expected)?;
    if source.completed_snapshot_handoff(&connection, &mut destination)? {
        return Ok(());
    }
    let bytes = source_bytes(&history, &receipt.intent)?;
    if let Some(check) = expected {
        check.history(&connection, &bytes)?;
    }
    source.fence(
        &connection,
        owner.authority.generation,
        &receipt.intent.handoff,
        &destination,
    )?;
    copy_history(&connection, &mut target_history, &receipt, &bytes)?;
    if let Some(expected) = expected {
        expected.roots(&[&source, &destination, &history, &target_history])?;
    }
    anyhow::ensure!(
        source_bytes(&history, &receipt.intent)? == bytes,
        "history changed during transfer"
    );
    finish(
        &source,
        &connection,
        &mut destination,
        &owner,
        &receipt.intent.handoff,
        expected,
    )
}

fn distinct(leases: &[&Lease; 4]) -> anyhow::Result<()> {
    let mut paths = std::collections::HashSet::new();
    let mut files = std::collections::HashSet::new();
    for lease in leases {
        #[cfg(windows)]
        let path = lease.path.as_os_str().to_ascii_lowercase();
        #[cfg(not(windows))]
        let path = lease.path.as_os_str().to_owned();
        anyhow::ensure!(paths.insert(path), "metadata paths overlap");
        if let Some(file) = &lease.file {
            let id = identity(file)?;
            anyhow::ensure!(
                !leases.iter().any(|other| other.lock_identity == id) && files.insert(id),
                "metadata files overlap"
            );
        }
    }
    Ok(())
}

fn prepare(
    connection: &turso::Connection,
    leases: &[&Lease; 4],
    owner: &Record,
    check: Option<TransferCheck<'_>>,
) -> anyhow::Result<Receipt> {
    let paths = leases
        .map(|lease| lease.path_text().map(str::to_owned))
        .into_iter()
        .collect::<anyhow::Result<Vec<_>>>()?;
    let paths: [String; 4] = paths.try_into().expect("four metadata paths");
    let locks = leases.map(|lease| lease.lock_identity.clone());
    if let Some(receipt) = load_receipt(connection)? {
        if receipt.intent.paths == paths && receipt.intent.locks == locks {
            return Ok(receipt);
        }
        anyhow::ensure!(
            !owner.fenced
                && receipt.complete
                && receipt.intent.paths[1] == paths[0]
                && receipt.intent.locks[1] == locks[0]
                && owner.handoff.as_deref() == Some(&receipt.intent.handoff),
            "metadata handoff paths changed"
        );
    }
    anyhow::ensure!(!owner.fenced, "another metadata handoff owns this catalog");
    anyhow::ensure!(
        leases[1].file.is_none() && leases[3].file.is_none(),
        "metadata destination exists"
    );
    let bytes = read_history(leases[2])?;
    if let Some(check) = check {
        check.history(connection, &bytes)?;
    }
    pollster::block_on(connection.execute_batch(HISTORY_SCHEMA))?;
    let intent = Intent {
        paths,
        locks,
        source_identity: leases[2].file_identity()?,
        bytes: u64::try_from(bytes.len())?,
        digest: Sha256::digest(&bytes).into(),
        handoff: uuid::Uuid::new_v4().to_string(),
    };
    durable(connection)?;
    pollster::block_on(connection.execute(
        "INSERT INTO recording_catalog_history_transfer(singleton,intent) VALUES(1,?1)
        ON CONFLICT(singleton) DO UPDATE SET intent=excluded.intent,destination_identity=NULL,complete=0",
        [serde_json::to_string(&intent)?],
    ))?;
    Ok(Receipt {
        intent,
        destination_identity: None,
        complete: false,
    })
}

fn load_receipt(connection: &turso::Connection) -> anyhow::Result<Option<Receipt>> {
    let mut tables = pollster::block_on(connection.query(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='recording_catalog_history_transfer'",
        (),
    ))?;
    if pollster::block_on(tables.next())?.is_none() {
        return Ok(None);
    }
    drop(tables);
    let mut rows = pollster::block_on(connection.query(
        "SELECT intent,destination_identity,complete FROM recording_catalog_history_transfer WHERE singleton=1",
        (),
    ))?;
    let Some(row) = pollster::block_on(rows.next())? else {
        return Ok(None);
    };
    let intent: Intent = serde_json::from_str(&row.get::<String>(0)?)?;
    anyhow::ensure!(
        intent.bytes <= HISTORY_BYTES_MAX,
        "history exceeds its size limit"
    );
    validate_handoff(1, &intent.handoff)?;
    Ok(Some(Receipt {
        intent,
        destination_identity: row.get(1)?,
        complete: row.get::<i64>(2)? != 0,
    }))
}

fn read_history(lease: &Lease) -> anyhow::Result<Vec<u8>> {
    lease.revalidate()?;
    let mut file = lease
        .file
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("export history is unavailable"))?
        .try_clone()?;
    anyhow::ensure!(
        file.metadata()?.len() <= HISTORY_BYTES_MAX,
        "history exceeds its size limit"
    );
    file.rewind()?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(HISTORY_BYTES_MAX + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        u64::try_from(bytes.len())? <= HISTORY_BYTES_MAX,
        "history exceeds its size limit"
    );
    lease.revalidate()?;
    Ok(bytes)
}

fn source_bytes(lease: &Lease, intent: &Intent) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        lease.file_identity()? == intent.source_identity,
        "source history identity changed"
    );
    let bytes = read_history(lease)?;
    anyhow::ensure!(
        u64::try_from(bytes.len())? == intent.bytes
            && <[u8; 32]>::from(Sha256::digest(&bytes)) == intent.digest,
        "source history contents changed"
    );
    Ok(bytes)
}

fn copy_history(
    connection: &turso::Connection,
    destination: &mut Lease,
    receipt: &Receipt,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let expected = match &receipt.destination_identity {
        Some(expected) => expected.clone(),
        None => register_history(connection, destination)?,
    };
    anyhow::ensure!(
        destination.file_identity()? == expected,
        "destination history identity changed"
    );
    if !receipt.complete {
        let mut write_options = options();
        write_options.write(true);
        let mut file = destination
            .directory
            .open_with(&destination.name, &write_options)?
            .into_std();
        anyhow::ensure!(
            identity(&file)? == expected,
            "destination history changed before writing"
        );
        destination.revalidate()?;
        file.set_len(0)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        destination.sync()?;
    }
    anyhow::ensure!(
        read_history(destination)? == bytes,
        "destination history contents changed"
    );
    pollster::block_on(connection.execute(
        "UPDATE recording_catalog_history_transfer SET complete=1 WHERE singleton=1 AND destination_identity=?1",
        [expected],
    ))?;
    Ok(())
}

fn register_history(
    connection: &turso::Connection,
    destination: &mut Lease,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        destination.file.is_none(),
        "unregistered destination history exists"
    );
    destination.revalidate()?;
    let mut create_options = options();
    create_options.write(true).create_new(true);
    let file = destination
        .directory
        .open_with(&destination.name, &create_options)?
        .into_std();
    let id = identity(&file)?;
    file.sync_all()?;
    destination.file = Some(file);
    destination.sync()?;
    pollster::block_on(connection.execute(
        "UPDATE recording_catalog_history_transfer SET destination_identity=?1 WHERE singleton=1 AND destination_identity IS NULL",
        [id.clone()],
    ))?;
    Ok(id)
}

fn finish(
    source: &Lease,
    connection: &turso::Connection,
    destination: &mut Lease,
    owner: &Record,
    handoff: &str,
    expected: Option<TransferCheck<'_>>,
) -> anyhow::Result<()> {
    use crate::backup::{BackupSection, database};
    source.snapshot_into(
        connection,
        destination,
        database::snapshot_size_limit(connection)?,
    )?;
    let copied = destination.connect()?;
    database::validate_connection(&copied, BackupSection::RecordingCatalog, true)?;
    if let Some(expected) = expected {
        destination.require_root(expected.destination)?;
        if let Some(root) = expected.source {
            source.require_root(root)?;
        }
    }
    destination.activate(
        &copied,
        source,
        connection,
        owner.authority.generation,
        handoff,
    )?;
    database::checkpoint(&copied)?;
    database::checkpoint(connection)?;
    destination.sync()?;
    source.sync()
}

#[cfg(test)]
mod tests;
