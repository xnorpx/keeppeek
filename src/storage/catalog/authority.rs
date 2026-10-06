//! Fences catalog writers and authorizes an offline transfer to a verified snapshot.

use cap_fs_ext::{FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use std::{
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::storage::long_term::inspection::removal::sync_directory;

mod metadata;
mod snapshot;
#[cfg(test)]
pub use metadata::transfer as transfer_metadata;
pub use metadata::{TransferCheck, transfer_checked as transfer_metadata_checked};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS recording_catalog_authority (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    catalog_id TEXT NOT NULL CHECK(length(catalog_id) BETWEEN 1 AND 36),
    generation INTEGER NOT NULL CHECK(typeof(generation) = 'integer' AND generation > 0),
    file_identity TEXT NOT NULL CHECK(length(file_identity) BETWEEN 1 AND 128),
    state INTEGER NOT NULL CHECK(state IN (0, 1)),
    handoff TEXT CHECK(handoff IS NULL OR length(handoff) BETWEEN 1 AND 36),
    destination TEXT CHECK(destination IS NULL OR length(destination) BETWEEN 1 AND 4096),
    destination_lock TEXT CHECK(destination_lock IS NULL OR length(destination_lock) BETWEEN 1 AND 128),
    import_id TEXT CHECK(import_id IS NULL OR length(import_id) BETWEEN 1 AND 128),
    CHECK((handoff IS NULL AND destination IS NULL AND destination_lock IS NULL AND state = 0)
       OR (handoff IS NOT NULL AND destination IS NOT NULL AND destination_lock IS NOT NULL))
)";

/// An exclusive bootstrap lease, retained until all database connections have closed.
pub struct Lease {
    path: PathBuf,
    directory: Dir,
    name: std::ffi::OsString,
    lock_name: std::ffi::OsString,
    lock: File,
    lock_identity: String,
    file: Option<File>,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lease([REDACTED])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authority {
    pub catalog_id: String,
    pub generation: u64,
}

pub struct MetadataInfo {
    pub authority: Authority,
    pub snapshot_bytes: u64,
}

#[derive(PartialEq, Eq)]
struct Record {
    authority: Authority,
    identity: String,
    fenced: bool,
    handoff: Option<String>,
    destination: Option<String>,
    destination_lock: Option<String>,
    import_id: Option<String>,
}

impl Lease {
    pub(crate) fn require_root(
        &self,
        root: &crate::storage::volumes::root::Root,
    ) -> anyhow::Result<()> {
        root.matches_directory(&self.directory)?;
        self.revalidate()
    }
    pub(crate) fn require_existing(&self) -> anyhow::Result<()> {
        self.revalidate()?;
        anyhow::ensure!(self.file.is_some(), "managed catalog is unavailable");
        Ok(())
    }

    pub(crate) fn sync(&self) -> anyhow::Result<()> {
        self.revalidate()?;
        sync_directory(&self.directory)?;
        self.revalidate()
    }

    /// Resumes the durable fence, or starts one for an authoritative compaction source.
    /// Foreign images are permitted only by the validated restore staging owner.
    pub(crate) fn snapshot_handoff(
        &self,
        connection: &turso::Connection,
        destination: &Self,
        imported: bool,
    ) -> anyhow::Result<Option<(Authority, String)>> {
        self.check_connection(connection)?;
        let mut rows = pollster::block_on(connection.query(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='recording_catalog_authority'",
            (),
        ))?;
        if pollster::block_on(rows.next())?.is_none() {
            return Ok(None);
        }
        drop(rows);
        let record = required(connection)?;
        if record.identity != self.file_identity()? {
            anyhow::ensure!(imported, "catalog compaction source is a copied image");
            return Ok(None);
        }
        let handoff = if record.fenced {
            record
                .handoff
                .clone()
                .ok_or_else(|| anyhow::anyhow!("catalog source fence is incomplete"))?
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        self.fence(
            connection,
            record.authority.generation,
            &handoff,
            destination,
        )?;
        Ok(Some((record.authority, handoff)))
    }

    /// Releases only the pinned leaf for an owner-controlled replacement under this lease.
    /// The caller must close all database connections before calling this method.
    pub(crate) fn release_file(&mut self) -> anyhow::Result<()> {
        self.revalidate()?;
        self.file = None;
        Ok(())
    }

    /// Binds a validated restore image to this file as a new local authority generation.
    /// Only the restore owner may call this after verifying its journal and image.
    pub(crate) fn import_validated(
        &self,
        connection: &turso::Connection,
        operation: &str,
    ) -> anyhow::Result<Authority> {
        anyhow::ensure!(
            !operation.is_empty() && operation.len() <= 128,
            "invalid catalog import identity"
        );
        self.check_connection(connection)?;
        reject_named_restore(connection)?;
        durable(connection)?;
        pollster::block_on(connection.execute(SCHEMA, ()))?;
        if load(connection)?.is_none() {
            self.initialize(connection)?;
        }
        let record = required(connection)?;
        if record.import_id.as_deref() == Some(operation) {
            return self.verify(connection);
        }
        let next = record
            .authority
            .generation
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| anyhow::anyhow!("catalog generation exhausted"))?;
        pollster::block_on(connection.execute(
            "UPDATE recording_catalog_authority SET generation=?1,file_identity=?2,state=0,
             handoff=NULL,destination=NULL,destination_lock=NULL,import_id=?3 WHERE singleton=1",
            turso::params![i64::try_from(next)?, self.file_identity()?, operation],
        ))?;
        self.verify(connection)
    }

    pub(crate) fn imported(
        &self,
        connection: &turso::Connection,
        operation: &str,
    ) -> anyhow::Result<bool> {
        self.check_connection(connection)?;
        let mut rows = pollster::block_on(connection.query(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='recording_catalog_authority'",
            (),
        ))?;
        if pollster::block_on(rows.next())?.is_none() {
            return Ok(false);
        }
        drop(rows);
        let record = required(connection)?;
        Ok(record.import_id.as_deref() == Some(operation)
            && !record.fenced
            && record.identity == self.file_identity()?)
    }

    /// Resolves legacy parent aliases and locks a stable sidecar in an existing directory.
    /// Missing catalog leaves are permitted for first creation and offline snapshots.
    pub(crate) fn acquire(path: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(
            path.as_os_str().len() <= 4_096,
            "catalog path exceeds its limit"
        );
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid catalog leaf"))?
            .to_owned();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = std::fs::canonicalize(parent)?;
        let directory = Dir::open_ambient_dir(&parent, cap_std::ambient_authority())?;
        let file = optional_file(&directory, &name)?;
        let mut lock_name = name.clone();
        lock_name.push(".authority.lock");
        let mut options = options();
        options.write(true).create(true);
        let lock = directory.open_with(&lock_name, &options)?.into_std();
        let lock_identity = identity(&lock)?;
        lock.try_lock()
            .map_err(|_| anyhow::anyhow!("catalog authority lease is unavailable"))?;
        lock.sync_all()?;
        sync_directory(&directory)?;
        let lease = Self {
            path: parent.join(&name),
            directory,
            name,
            lock_name,
            lock,
            lock_identity,
            file,
        };
        lease.revalidate()?;
        Ok(lease)
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Opens a database after acquiring the lease and pinning its leaf.
    /// Callers must close the database and its connections before releasing this lease.
    pub(crate) fn database(&mut self) -> anyhow::Result<turso::Database> {
        self.revalidate()?;
        if self.file.is_none() {
            let existing = optional_file(&self.directory, &self.name)?;
            let file = match existing {
                Some(file) => file,
                None => {
                    let mut options = options();
                    options.write(true).create_new(true);
                    let file = self.directory.open_with(&self.name, &options)?.into_std();
                    identity(&file)?;
                    file.sync_all()?;
                    sync_directory(&self.directory)?;
                    file
                }
            };
            self.file = Some(file);
        }
        self.revalidate()?;
        let path = self
            .path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid catalog path encoding"))?;
        let database = pollster::block_on(
            turso::Builder::new_local(path)
                .experimental_vacuum(true)
                .experimental_generated_columns(true)
                .build(),
        )?;
        self.revalidate()?;
        Ok(database)
    }

    /// Opens one offline connection while retaining the lease in this value.
    pub(crate) fn connect(&mut self) -> anyhow::Result<turso::Connection> {
        let connection = self.database()?.connect()?;
        connection.busy_timeout(Duration::from_secs(2))?;
        self.check_connection(&connection)?;
        Ok(connection)
    }

    /// Adopts a legacy catalog, or verifies its existing authority without resetting a fence.
    pub(crate) fn initialize(&self, connection: &turso::Connection) -> anyhow::Result<Authority> {
        self.check_connection(connection)?;
        durable(connection)?;
        pollster::block_on(connection.execute(SCHEMA, ()))?;
        if load(connection)?.is_none() {
            pollster::block_on(connection.execute(
                "INSERT INTO recording_catalog_authority(singleton,catalog_id,generation,file_identity,state)
                 VALUES (1,?1,1,?2,0)",
                turso::params![uuid::Uuid::new_v4().to_string(), self.file_identity()?],
            ))?;
        }
        self.verify(connection)
    }

    /// Refuses retained, copied, or replaced catalogs before ordinary schema initialization.
    pub(crate) fn verify(&self, connection: &turso::Connection) -> anyhow::Result<Authority> {
        self.check_connection(connection)?;
        self.verify_record(connection)
    }

    /// Checks authority while the caller holds a catalog write transaction.
    pub(crate) fn verify_transaction(
        &self,
        connection: &turso::Connection,
    ) -> anyhow::Result<Authority> {
        anyhow::ensure!(
            !connection.is_autocommit()?,
            "catalog authority requires an active transaction"
        );
        self.check_connection_identity(connection)?;
        self.verify_record(connection)
    }

    fn verify_record(&self, connection: &turso::Connection) -> anyhow::Result<Authority> {
        let record = required(connection)?;
        anyhow::ensure!(
            !record.fenced,
            "catalog authority is fenced for offline handoff"
        );
        anyhow::ensure!(
            record.identity == self.file_identity()?,
            "catalog authority file identity changed"
        );
        Ok(record.authority)
    }

    /// Fences the source before snapshotting. Exact retries retain the original destination.
    pub(crate) fn fence(
        &self,
        connection: &turso::Connection,
        generation: u64,
        handoff: &str,
        destination: &Self,
    ) -> anyhow::Result<()> {
        validate_handoff(generation, handoff)?;
        self.check_connection(connection)?;
        destination.revalidate()?;
        let record = required(connection)?;
        anyhow::ensure!(
            record.authority.generation == generation && record.identity == self.file_identity()?,
            "catalog handoff source changed"
        );
        if record.fenced {
            return matches_handoff(&record, handoff, destination);
        }
        anyhow::ensure!(
            record.handoff.as_deref() != Some(handoff),
            "catalog handoff identity was already used"
        );
        anyhow::ensure!(
            self.lock_identity != destination.lock_identity,
            "catalog handoff requires another destination"
        );
        anyhow::ensure!(
            optional_file(&destination.directory, &destination.name)?.is_none(),
            "catalog handoff destination already exists"
        );
        durable(connection)?;
        pollster::block_on(connection.execute_batch("BEGIN IMMEDIATE"))?;
        let result = pollster::block_on(async {
            install_format_barrier(connection).await?;
            let changed = connection.execute(
            "UPDATE recording_catalog_authority SET state=1,handoff=?1,destination=?2,destination_lock=?3
             WHERE singleton=1 AND state=0 AND generation=?4",
            turso::params![handoff, destination.path_text()?, destination.lock_identity.as_str(), i64::try_from(generation)?],
            ).await?;
            anyhow::ensure!(changed == 1, "catalog handoff source changed");
            connection.execute_batch("COMMIT").await?;
            anyhow::Ok(())
        });
        if let Err(error) = result {
            if !connection.is_autocommit()? {
                pollster::block_on(connection.execute_batch("ROLLBACK"))?;
            }
            return Err(error);
        }
        self.revalidate()
    }

    /// Activates a caller-verified snapshot while both source and destination leases are held.
    /// The source remains fenced. Snapshot integrity and quiescence belong to bootstrap.
    pub(crate) fn activate(
        &self,
        connection: &turso::Connection,
        source: &Self,
        source_connection: &turso::Connection,
        generation: u64,
        handoff: &str,
    ) -> anyhow::Result<Authority> {
        validate_handoff(generation, handoff)?;
        source.check_connection(source_connection)?;
        self.check_connection(connection)?;
        let original = required(source_connection)?;
        anyhow::ensure!(
            original.fenced
                && original.authority.generation == generation
                && original.identity == source.file_identity()?,
            "catalog source fence changed"
        );
        matches_handoff(&original, handoff, self)?;
        let copied = required(connection)?;
        let next = generation
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| anyhow::anyhow!("catalog generation exhausted"))?;
        if !copied.fenced {
            anyhow::ensure!(
                copied.authority.catalog_id == original.authority.catalog_id
                    && copied.authority.generation == next,
                "catalog destination authority changed"
            );
            matches_handoff(&copied, handoff, self)?;
            return self.verify(connection);
        }
        anyhow::ensure!(
            copied == original,
            "catalog snapshot does not match source fence"
        );
        durable(connection)?;
        let changed = pollster::block_on(connection.execute(
            "UPDATE recording_catalog_authority SET state=0,generation=?1,file_identity=?2
             WHERE singleton=1 AND state=1 AND generation=?3 AND handoff=?4",
            turso::params![
                i64::try_from(next)?,
                self.file_identity()?,
                i64::try_from(generation)?,
                handoff
            ],
        ))?;
        anyhow::ensure!(changed == 1, "catalog destination authority changed");
        self.verify(connection)
    }

    fn path_text(&self) -> anyhow::Result<&str> {
        self.path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid catalog path encoding"))
    }

    fn file_identity(&self) -> anyhow::Result<String> {
        identity(
            self.file
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("catalog leaf is not pinned"))?,
        )
    }

    fn revalidate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            identity(&self.lock)? == self.lock_identity,
            "catalog lease identity changed"
        );
        let named = self
            .directory
            .open_with(&self.lock_name, &options())?
            .into_std();
        anyhow::ensure!(
            identity(&named)? == self.lock_identity,
            "catalog lease name changed"
        );
        let current = Dir::open_ambient_dir(
            self.path.parent().expect("canonical catalog parent"),
            cap_std::ambient_authority(),
        )?;
        let current_lock = current.open_with(&self.lock_name, &options())?.into_std();
        anyhow::ensure!(
            identity(&current_lock)? == self.lock_identity,
            "catalog parent changed"
        );
        if let Some(file) = &self.file {
            let named = self.directory.open_with(&self.name, &options())?.into_std();
            anyhow::ensure!(identity(&named)? == identity(file)?, "catalog leaf changed");
        }
        Ok(())
    }

    fn check_connection(&self, connection: &turso::Connection) -> anyhow::Result<()> {
        anyhow::ensure!(
            connection.is_autocommit()?,
            "catalog authority requires an independent transaction"
        );
        self.check_connection_identity(connection)
    }

    fn check_connection_identity(&self, connection: &turso::Connection) -> anyhow::Result<()> {
        self.revalidate()?;
        let mut rows = pollster::block_on(connection.query("PRAGMA database_list", ()))?;
        for _ in 0..16 {
            let Some(row) = pollster::block_on(rows.next())? else {
                break;
            };
            if row.get::<String>(1)? == "main" {
                let path = PathBuf::from(row.get::<String>(2)?);
                anyhow::ensure!(
                    std::fs::canonicalize(path)? == std::fs::canonicalize(&self.path)?,
                    "catalog connection does not own this lease"
                );
                return Ok(());
            }
        }
        anyhow::bail!("catalog connection has no bounded main database")
    }
}

fn validate_handoff(generation: u64, handoff: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        generation > 0
            && generation < i64::MAX as u64
            && handoff.len() <= 36
            && uuid::Uuid::parse_str(handoff).is_ok(),
        "invalid catalog handoff identity"
    );
    Ok(())
}

fn durable(connection: &turso::Connection) -> anyhow::Result<()> {
    anyhow::ensure!(
        connection.is_autocommit()?,
        "catalog authority requires an independent transaction"
    );
    pollster::block_on(connection.execute("PRAGMA synchronous=FULL", ()))?;
    Ok(())
}

fn matches_handoff(record: &Record, handoff: &str, destination: &Lease) -> anyhow::Result<()> {
    anyhow::ensure!(
        record.handoff.as_deref() == Some(handoff)
            && record.destination.as_deref() == Some(destination.path_text()?)
            && record.destination_lock.as_deref() == Some(destination.lock_identity.as_str()),
        "catalog handoff destination changed"
    );
    Ok(())
}

fn required(connection: &turso::Connection) -> anyhow::Result<Record> {
    load(connection)?.ok_or_else(|| anyhow::anyhow!("catalog authority is missing"))
}

fn load(connection: &turso::Connection) -> anyhow::Result<Option<Record>> {
    let mut rows = pollster::block_on(connection.query(
        "SELECT catalog_id,generation,file_identity,state,handoff,destination,destination_lock,import_id
         FROM recording_catalog_authority WHERE singleton=1",
        (),
    ))?;
    let Some(row) = pollster::block_on(rows.next())? else {
        return Ok(None);
    };
    let state = row.get::<i64>(3)?;
    anyhow::ensure!(matches!(state, 0 | 1), "catalog authority state is invalid");
    let record = Record {
        authority: Authority {
            catalog_id: row.get(0)?,
            generation: u64::try_from(row.get::<i64>(1)?)?,
        },
        identity: row.get(2)?,
        fenced: state == 1,
        handoff: row.get(4)?,
        destination: row.get(5)?,
        destination_lock: row.get(6)?,
        import_id: row.get(7)?,
    };
    anyhow::ensure!(
        record.authority.generation > 0
            && record.authority.catalog_id.len() <= 36
            && uuid::Uuid::parse_str(&record.authority.catalog_id).is_ok()
            && record.identity.len() <= 128
            && record.handoff.as_ref().is_none_or(|s| s.len() <= 36)
            && record.destination.as_ref().is_none_or(|s| s.len() <= 4_096)
            && record
                .destination_lock
                .as_ref()
                .is_none_or(|s| s.len() <= 128),
        "catalog authority record is invalid"
    );
    let empty_handoff = record.handoff.is_none()
        && record.destination.is_none()
        && record.destination_lock.is_none();
    let full_handoff = record
        .handoff
        .as_deref()
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        && record
            .destination
            .as_ref()
            .is_some_and(|path| !path.is_empty())
        && record
            .destination_lock
            .as_ref()
            .is_some_and(|id| !id.is_empty());
    anyhow::ensure!(
        full_handoff || (empty_handoff && !record.fenced),
        "catalog authority handoff is invalid"
    );
    Ok(Some(record))
}

fn optional_file(directory: &Dir, name: &std::ffi::OsStr) -> anyhow::Result<Option<File>> {
    match directory.open_with(name, &options()) {
        Ok(file) => {
            let file = file.into_std();
            identity(&file)?;
            Ok(Some(file))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_fs_ext::OpenOptionsSyncExt;
        use cap_std::fs::OpenOptionsExt;
        options.nonblock(true);
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        options.share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0);
    }
    options
}

fn identity(file: &File) -> anyhow::Result<String> {
    let metadata = cap_std::fs::File::from_std(file.try_clone()?).metadata()?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.nlink() == 1,
        "catalog leaf must be a regular file with one link"
    );
    physical_identity(file)
}

fn physical_identity(file: &File) -> anyhow::Result<String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{
                FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_ID_INFO,
                FileAttributeTagInfo, FileIdInfo, GetFileInformationByHandleEx,
            },
        };
        let mut id = FILE_ID_INFO::default();
        let mut attributes = FILE_ATTRIBUTE_TAG_INFO::default();
        // SAFETY: The handle is pinned and each output has its declared size.
        unsafe {
            GetFileInformationByHandleEx(
                HANDLE(file.as_raw_handle()),
                FileIdInfo,
                std::ptr::from_mut(&mut id).cast(),
                u32::try_from(size_of::<FILE_ID_INFO>())?,
            )?;
            GetFileInformationByHandleEx(
                HANDLE(file.as_raw_handle()),
                FileAttributeTagInfo,
                std::ptr::from_mut(&mut attributes).cast(),
                u32::try_from(size_of::<FILE_ATTRIBUTE_TAG_INFO>())?,
            )?;
        }
        anyhow::ensure!(
            attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
            "catalog leaf cannot be a reparse point"
        );
        Ok(format!(
            "{}:{:032x}",
            id.VolumeSerialNumber,
            u128::from_be_bytes(id.FileId.Identifier)
        ))
    }
    #[cfg(unix)]
    {
        let metadata = cap_std::fs::File::from_std(file.try_clone()?).metadata()?;
        Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        anyhow::bail!("catalog identity is unsupported")
    }
}

#[cfg(test)]
mod tests;

/// Transfers a stopped legacy catalog and retains its source as a durable fence.
pub fn transfer_legacy(source_path: &Path, destination_path: &Path) -> anyhow::Result<()> {
    use crate::backup::{BackupSection, database};
    let mut source = Lease::acquire(source_path)?;
    let mut destination = Lease::acquire(destination_path)?;
    let source_connection = source.connect()?;
    pollster::block_on(source_connection.execute(SCHEMA, ()))?;
    if load(&source_connection)?.is_none() {
        source.initialize(&source_connection)?;
    }
    reject_named_restore(&source_connection)?;
    let record = required(&source_connection)?;
    let handoff = record
        .handoff
        .clone()
        .filter(|_| record.fenced)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    source.fence(
        &source_connection,
        record.authority.generation,
        &handoff,
        &destination,
    )?;
    source.snapshot_into(
        &source_connection,
        &mut destination,
        database::snapshot_size_limit(&source_connection)?,
    )?;
    let connection = destination.connect()?;
    database::validate_connection(&connection, BackupSection::RecordingCatalog, true)?;
    destination.activate(
        &connection,
        &source,
        &source_connection,
        record.authority.generation,
        &handoff,
    )?;
    database::checkpoint(&connection)?;
    database::checkpoint(&source_connection)?;
    destination.sync()?;
    drop(connection);
    drop(source_connection);
    sync_directory(&source.directory)?;
    Ok(())
}

pub fn reject_named_restore(connection: &turso::Connection) -> anyhow::Result<()> {
    let mut rows = pollster::block_on(connection.query(
        "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='storage_volume_bindings'",
        (),
    ))?;
    if pollster::block_on(rows.next())?.is_none() {
        return Ok(());
    }
    drop(rows);
    let mut rows =
        pollster::block_on(connection.query("SELECT 1 FROM storage_volume_bindings LIMIT 1", ()))?;
    anyhow::ensure!(
        pollster::block_on(rows.next())?.is_none(),
        "restoring named storage ownership requires an explicit root ownership transfer"
    );
    Ok(())
}

/// Makes historical builders reject schema loading before they can ignore ownership metadata.
/// The caller must install the barrier in the transaction that first requires the new format.
pub async fn install_format_barrier(connection: &turso::Connection) -> anyhow::Result<()> {
    anyhow::ensure!(
        !connection.is_autocommit()?,
        "catalog format barrier requires an ownership transaction"
    );
    // Turso 0.7.2 refuses this schema unless the opener enables generated columns.
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS recording_catalog_requires_volume_authority (
        version INTEGER NOT NULL CHECK(version=1), reader_generation INTEGER AS(version) VIRTUAL
    )",
        )
        .await?;
    let mut rows = connection
        .query(
            "PRAGMA table_xinfo(recording_catalog_requires_volume_authority)",
            (),
        )
        .await?;
    let first = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("catalog format barrier is missing"))?;
    let guard = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("catalog format barrier is incomplete"))?;
    anyhow::ensure!(
        first.get::<String>(1)? == "version"
            && guard.get::<String>(1)? == "reader_generation"
            && guard.get::<i64>(6)? == 2
            && rows.next().await?.is_none(),
        "catalog format barrier has an incompatible schema"
    );
    Ok(())
}
