use std::path::Path;

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

pub fn remove_database_family(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(sidecar));
    }
}
