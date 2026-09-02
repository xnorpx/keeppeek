use std::path::Path;

use anyhow::Context as _;

use super::BackupSection;

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

pub fn compact_turso_database(
    path: &Path,
    temporary: &Path,
    maximum_bytes: u64,
) -> anyhow::Result<()> {
    remove_database_family(temporary);
    let result = (|| {
        snapshot_turso_database_path(path, temporary, maximum_bytes)?;
        remove_database_family(path);
        std::fs::rename(temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        remove_database_family(temporary);
    }
    result
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
    let database = pollster::block_on(turso::Builder::new_local(path_text).build())
        .with_context(|| format!("invalid {} database", section.as_str()))?;
    let connection = database.connect()?;
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
        for table in required_tables(section) {
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
        BackupSection::Notifications => &["notification_rules", "notification_rule_versions"],
        _ => &[],
    }
}
