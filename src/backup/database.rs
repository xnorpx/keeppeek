use std::path::Path;

use anyhow::Context as _;

use super::BackupSection;

#[cfg(test)]
pub const DATABASE_SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Creates a durable point-in-time database copy through Turso's native `VACUUM INTO` path.
pub fn snapshot_turso_database(
    connection: &turso::Connection,
    destination: &Path,
    maximum_bytes: u64,
) -> anyhow::Result<u64> {
    if maximum_bytes == 0 {
        anyhow::bail!("database snapshot size limit must be nonzero");
    }
    if destination.exists() {
        anyhow::bail!(
            "database snapshot destination already exists: {}",
            destination.display()
        );
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let destination = destination
        .to_str()
        .filter(|path| !path.contains('\0'))
        .ok_or_else(|| anyhow::anyhow!("database snapshot path is not valid UTF-8"))?;
    let quoted_destination = destination.replace('\'', "''");
    let statement = format!("VACUUM INTO '{quoted_destination}'");
    if let Err(error) = pollster::block_on(connection.execute(&statement, ())) {
        remove_database_family(Path::new(destination));
        return Err(error.into());
    }
    let bytes = std::fs::metadata(destination)?.len();
    if bytes == 0 || bytes > maximum_bytes {
        remove_database_family(Path::new(destination));
        anyhow::bail!("database snapshot exceeds its size limit");
    }
    Ok(bytes)
}

pub fn snapshot_turso_database_path(
    source: &Path,
    destination: &Path,
    maximum_bytes: u64,
) -> anyhow::Result<u64> {
    let metadata = std::fs::symlink_metadata(source)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        anyhow::bail!("database snapshot source is not a regular file");
    }
    let source = source
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("database snapshot source path is not valid UTF-8"))?;
    let database = pollster::block_on(
        turso::Builder::new_local(source)
            .experimental_vacuum(true)
            .experimental_generated_columns(true)
            .build(),
    )?;
    let connection = database.connect()?;
    let bytes = snapshot_turso_database(&connection, destination, maximum_bytes)?;
    drop(connection);
    drop(database);
    remove_database_sidecars(destination);
    std::fs::File::options()
        .write(true)
        .open(destination)?
        .sync_all()?;
    Ok(bytes)
}

#[cfg(test)]
pub fn compact_turso_database(
    path: &Path,
    temporary: &Path,
    maximum_bytes: u64,
) -> anyhow::Result<()> {
    compact_database(path, temporary, maximum_bytes, false)
}

pub(super) fn compact_restore_database(
    path: &Path,
    temporary: &Path,
    maximum_bytes: u64,
) -> anyhow::Result<()> {
    compact_database(path, temporary, maximum_bytes, true)
}

fn compact_database(
    path: &Path,
    temporary: &Path,
    maximum_bytes: u64,
    imported: bool,
) -> anyhow::Result<()> {
    use crate::storage::catalog::authority::Lease;
    let mut source = Lease::acquire(path)?;
    let mut target = Lease::acquire(temporary)?;
    let connection = source.connect()?;
    validate_connection(&connection, BackupSection::RecordingCatalog, false)?;
    let transfer = source.snapshot_handoff(&connection, &target, imported)?;
    source.snapshot_into(&connection, &mut target, maximum_bytes)?;
    let target_connection = target.connect()?;
    validate_connection(&target_connection, BackupSection::RecordingCatalog, false)?;
    if let Some((owner, handoff)) = transfer {
        target.activate(
            &target_connection,
            &source,
            &connection,
            owner.generation,
            &handoff,
        )?;
    }
    checkpoint(&target_connection)?;
    checkpoint(&connection)?;
    drop(target_connection);
    drop(connection);
    source.release_file()?;
    target.release_file()?;
    remove_database_sidecars_checked(path)?;
    replace_database_file(temporary, path)?;
    std::fs::File::options()
        .write(true)
        .open(path)?
        .sync_all()?;
    source.sync()?;
    target.sync()?;
    Ok(())
}

fn remove_database_sidecars_checked(path: &Path) -> anyhow::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let mut member = path.as_os_str().to_owned();
        member.push(suffix);
        match std::fs::remove_file(std::path::PathBuf::from(member)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn replace_database_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            },
            core::PCWSTR,
        };
        let source = source
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let destination = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        // SAFETY: Both paths are terminated UTF-16 buffers retained across the call.
        unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(destination.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )?;
        }
    }
    #[cfg(not(windows))]
    std::fs::rename(source, destination)?;
    Ok(())
}

pub fn remove_database_sidecars(path: &Path) {
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(sidecar));
    }
}

pub fn remove_database_family(path: &Path) {
    let _ = std::fs::remove_file(path);
    remove_database_sidecars(path);
}

pub(super) fn validate_backup_database(path: &Path, section: BackupSection) -> anyhow::Result<()> {
    let path_text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("backup database path is not valid UTF-8"))?;
    let database = pollster::block_on(
        turso::Builder::new_local(path_text)
            .experimental_generated_columns(true)
            .build(),
    )
    .with_context(|| format!("invalid {} database", section.as_str()))?;
    let connection = database.connect()?;
    validate_connection(&connection, section, true)
}

pub fn validate_connection(
    connection: &turso::Connection,
    section: BackupSection,
    require_tables: bool,
) -> anyhow::Result<()> {
    pollster::block_on(async {
        let mut rows = connection
            .query("PRAGMA quick_check(1)", ())
            .await
            .with_context(|| format!("invalid {} database", section.as_str()))?;
        let result = rows
            .next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("database integrity check returned no result"))?
            .get::<String>(0)?;
        if result != "ok" {
            anyhow::bail!("database integrity check failed: {result}");
        }
        for table in required_tables(section).iter().filter(|_| require_tables) {
            let mut rows = connection
                .query(
                    "SELECT 1 FROM sqlite_schema
                     WHERE type = 'table' AND name = ?1 LIMIT 1",
                    turso::params![*table],
                )
                .await?;
            if rows.next().await?.is_none() {
                anyhow::bail!("required table {table} is missing");
            }
        }
        anyhow::Ok(())
    })
    .with_context(|| format!("invalid {} database", section.as_str()))
}

const fn required_tables(section: BackupSection) -> &'static [&'static str] {
    match section {
        BackupSection::RecordingCatalog => &["recording_files", "recording_events"],
        _ => &[],
    }
}

pub fn checkpoint(connection: &turso::Connection) -> anyhow::Result<()> {
    let mut rows = pollster::block_on(connection.query("PRAGMA wal_checkpoint(TRUNCATE)", ()))?;
    let row = pollster::block_on(rows.next())?
        .ok_or_else(|| anyhow::anyhow!("database checkpoint returned no result"))?;
    anyhow::ensure!(row.get::<i64>(0)? == 0, "database checkpoint is busy");
    Ok(())
}

pub fn snapshot_size_limit(connection: &turso::Connection) -> anyhow::Result<u64> {
    let mut bytes = 2_u64;
    for pragma in ["PRAGMA page_count", "PRAGMA page_size"] {
        let mut rows = pollster::block_on(connection.query(pragma, ()))?;
        let row = pollster::block_on(rows.next())?
            .ok_or_else(|| anyhow::anyhow!("database size is unavailable"))?;
        let value = u64::try_from(row.get::<i64>(0)?)?;
        bytes = bytes
            .checked_mul(value)
            .ok_or_else(|| anyhow::anyhow!("database size overflow"))?;
    }
    anyhow::ensure!(bytes > 0, "database size is unavailable");
    Ok(bytes)
}
