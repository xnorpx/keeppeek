use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek};
use std::time::Instant;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS recording_catalog_snapshot (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1), source_identity TEXT NOT NULL,
    destination TEXT NOT NULL, destination_lock TEXT NOT NULL, file_identity TEXT NOT NULL,
    scratch_directory TEXT NOT NULL,
    digest BLOB CHECK(digest IS NULL OR length(digest)=32))";

struct Snapshot {
    identity: String,
    scratch: String,
    digest: Option<Vec<u8>>,
}

impl Lease {
    /// Resumes only the captured output leaf. An unknown existing file is never reset.
    pub fn snapshot_into(
        &self,
        connection: &turso::Connection,
        destination: &mut Self,
        maximum_bytes: u64,
    ) -> anyhow::Result<()> {
        self.check_connection(connection)?;
        destination.revalidate()?;
        durable(connection)?;
        pollster::block_on(connection.execute(SCHEMA, ()))?;
        pollster::block_on(connection.execute(
            "DELETE FROM recording_catalog_snapshot WHERE source_identity<>?1",
            turso::params![self.file_identity()?],
        ))?;
        let snapshot = match self.snapshot_record(connection, destination)? {
            Some(snapshot) => snapshot,
            None if destination.path.exists() => return Ok(()),
            None => self.register_snapshot(connection, destination)?,
        };
        destination.pin_existing()?;
        anyhow::ensure!(
            destination.file_identity()? == snapshot.identity,
            "snapshot output identity changed"
        );
        if snapshot.digest.is_some() && destination.completed_authority(connection, self)? {
            destination.cleanup_scratch(&snapshot)?;
            return Ok(());
        }
        if let Some(expected) = &snapshot.digest {
            anyhow::ensure!(
                destination.snapshot_digest(maximum_bytes)?.as_slice() == expected.as_slice(),
                "snapshot output contents changed"
            );
            destination.cleanup_scratch(&snapshot)?;
            return Ok(());
        }
        destination.reset_snapshot()?;
        destination.populate_snapshot(connection, &snapshot, maximum_bytes)?;
        destination.sync()?;
        let digest = destination.snapshot_digest(maximum_bytes)?;
        pollster::block_on(connection.execute("UPDATE recording_catalog_snapshot SET digest=?1 WHERE singleton=1 AND file_identity=?2", turso::params![digest.as_slice(), snapshot.identity.as_str()]))?;
        destination.cleanup_scratch(&snapshot)?;
        Ok(())
    }

    fn snapshot_record(
        &self,
        connection: &turso::Connection,
        destination: &Self,
    ) -> anyhow::Result<Option<Snapshot>> {
        let mut rows = pollster::block_on(connection.query("SELECT destination,destination_lock,file_identity,digest,scratch_directory FROM recording_catalog_snapshot WHERE singleton=1", ()))?;
        let Some(row) = pollster::block_on(rows.next())? else {
            return Ok(None);
        };
        anyhow::ensure!(
            row.get::<String>(0)? == destination.path_text()?
                && row.get::<String>(1)? == destination.lock_identity,
            "snapshot destination changed"
        );
        let snapshot = Snapshot {
            identity: row.get(2)?,
            digest: row.get(3)?,
            scratch: row.get(4)?,
        };
        anyhow::ensure!(
            snapshot.identity.len() <= 128
                && snapshot
                    .digest
                    .as_ref()
                    .is_none_or(|digest| digest.len() == 32),
            "invalid snapshot record"
        );
        Ok(Some(snapshot))
    }

    fn register_snapshot(
        &self,
        connection: &turso::Connection,
        destination: &mut Self,
    ) -> anyhow::Result<Snapshot> {
        let mut name = destination.name.clone();
        name.push(".snapshot");
        let directory = crate::storage::long_term::inspection::removal::create_private_directory(
            &destination.directory,
            &name,
        )?;
        let scratch = physical_identity(&directory.try_clone()?.into_std_file())?;
        let mut options = options();
        options.write(true).create_new(true);
        let file = destination
            .directory
            .open_with(&destination.name, &options)?
            .into_std();
        let identity = identity(&file)?;
        file.sync_all()?;
        destination.file = Some(file);
        destination.sync()?;
        pollster::block_on(connection.execute(
            "INSERT INTO recording_catalog_snapshot VALUES(1,?1,?2,?3,?4,?5,NULL)",
            turso::params![
                self.file_identity()?,
                destination.path_text()?,
                destination.lock_identity.as_str(),
                identity.as_str(),
                scratch.as_str()
            ],
        ))?;
        Ok(Snapshot {
            identity,
            scratch,
            digest: None,
        })
    }

    fn scratch_path(&self) -> PathBuf {
        let mut name = self.name.clone();
        name.push(".snapshot");
        self.path.parent().expect("canonical parent").join(name)
    }

    fn open_scratch(&self) -> anyhow::Result<Dir> {
        use cap_fs_ext::DirExt;
        let mut name = self.name.clone();
        name.push(".snapshot");
        let directory = self.directory.open_dir_nofollow(name)?;
        crate::storage::long_term::inspection::removal::validate_owner(&directory, 0o077)?;
        Ok(directory)
    }

    fn cleanup_scratch(&self, snapshot: &Snapshot) -> anyhow::Result<()> {
        let path = self.scratch_path();
        if !path.try_exists()? {
            return Ok(());
        }
        let directory = self.open_scratch()?;
        anyhow::ensure!(
            physical_identity(&directory.try_clone()?.into_std_file())? == snapshot.scratch,
            "snapshot staging directory changed"
        );
        for name in ["image", "image-wal", "image-shm"] {
            self.validate_scratch(&directory, &snapshot.scratch)?;
            let existing = optional_file(&directory, name.as_ref())?;
            if existing.is_some() {
                drop(existing);
                directory.remove_file(name)?;
            }
        }
        anyhow::ensure!(
            physical_identity(&self.open_scratch()?.into_std_file())? == snapshot.scratch,
            "snapshot staging directory changed"
        );
        self.remove_scratch_directory(directory, &snapshot.scratch)?;
        self.sync()
    }

    fn remove_scratch_directory(&self, directory: Dir, expected: &str) -> anyhow::Result<()> {
        self.validate_scratch(&directory, expected)?;
        #[cfg(windows)]
        {
            use cap_fs_ext::OpenOptionsMaybeDirExt;
            use cap_std::fs::OpenOptionsExt;
            use windows::Win32::{
                Foundation::GENERIC_READ,
                Storage::FileSystem::{
                    DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
                    FILE_SHARE_READ, FILE_SHARE_WRITE,
                },
            };
            let mut options = OpenOptions::new();
            options
                .read(true)
                .maybe_dir(true)
                .access_mode(GENERIC_READ.0 | DELETE.0)
                .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
                .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0);
            drop(directory);
            let mut name = self.name.clone();
            name.push(".snapshot");
            let file = self.directory.open_with(name, &options)?;
            anyhow::ensure!(
                physical_identity(&file.try_clone()?.into_std())? == expected,
                "snapshot staging directory changed"
            );
            crate::storage::long_term::inspection::removal::windows::remove(file)?;
        }
        #[cfg(not(windows))]
        {
            let mut name = self.name.clone();
            name.push(".snapshot");
            self.directory.remove_dir(name)?;
        }
        Ok(())
    }

    fn validate_scratch(&self, directory: &Dir, expected: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            physical_identity(&directory.try_clone()?.into_std_file())? == expected
                && physical_identity(&self.open_scratch()?.into_std_file())? == expected,
            "snapshot staging directory changed"
        );
        self.revalidate()
    }

    fn populate_snapshot(
        &self,
        connection: &turso::Connection,
        snapshot: &Snapshot,
        maximum_bytes: u64,
    ) -> anyhow::Result<()> {
        let scratch_path = self.scratch_path();
        let directory = self.open_scratch()?;
        anyhow::ensure!(
            physical_identity(&directory.try_clone()?.into_std_file())? == snapshot.scratch,
            "snapshot staging directory changed"
        );
        for name in ["image", "image-wal", "image-shm"] {
            self.validate_scratch(&directory, &snapshot.scratch)?;
            let existing = optional_file(&directory, name.as_ref())?;
            if existing.is_some() {
                drop(existing);
                directory.remove_file(name)?;
            }
        }
        anyhow::ensure!(
            physical_identity(&self.open_scratch()?.into_std_file())? == snapshot.scratch,
            "snapshot staging directory changed"
        );
        crate::backup::database::snapshot_turso_database(
            connection,
            &scratch_path.join("image"),
            maximum_bytes,
        )?;
        anyhow::ensure!(
            physical_identity(&self.open_scratch()?.into_std_file())? == snapshot.scratch,
            "snapshot staging directory changed"
        );
        let mut input = directory.open_with("image", &options())?.into_std();
        identity(&input)?;
        let mut write_options = options();
        write_options.write(true);
        let mut output = self
            .directory
            .open_with(&self.name, &write_options)?
            .into_std();
        anyhow::ensure!(
            identity(&output)? == snapshot.identity,
            "snapshot output changed"
        );
        let bytes = std::io::copy(
            &mut (&mut input).take(
                maximum_bytes
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("snapshot bound overflow"))?,
            ),
            &mut output,
        )?;
        anyhow::ensure!(
            bytes > 0 && bytes <= maximum_bytes,
            "snapshot exceeds size bound"
        );
        output.sync_all()?;
        anyhow::ensure!(
            physical_identity(&self.open_scratch()?.into_std_file())? == snapshot.scratch,
            "snapshot staging directory changed"
        );
        self.revalidate()
    }

    fn pin_existing(&mut self) -> anyhow::Result<()> {
        if self.file.is_none() {
            self.file = optional_file(&self.directory, &self.name)?;
        }
        anyhow::ensure!(self.file.is_some(), "snapshot output is missing");
        self.revalidate()
    }

    fn reset_snapshot(&self) -> anyhow::Result<()> {
        self.revalidate()?;
        let mut options = options();
        options.write(true);
        let file = self.directory.open_with(&self.name, &options)?.into_std();
        anyhow::ensure!(
            identity(&file)? == self.file_identity()?,
            "snapshot output changed"
        );
        file.set_len(0)?;
        file.sync_all()?;
        for suffix in ["-wal", "-shm"] {
            let mut name = self.name.clone();
            name.push(suffix);
            let existing = optional_file(&self.directory, &name)?;
            if existing.is_some() {
                drop(existing);
                self.directory.remove_file(name)?;
            }
        }
        self.sync()
    }

    fn snapshot_digest(&self, maximum_bytes: u64) -> anyhow::Result<[u8; 32]> {
        let mut file = self.file.as_ref().expect("pinned snapshot").try_clone()?;
        let mut remaining = file.metadata()?.len();
        anyhow::ensure!(
            remaining > 0 && remaining <= maximum_bytes,
            "snapshot output exceeds size bound"
        );
        file.rewind()?;
        let deadline = Instant::now() + Duration::from_secs(300);
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 65_536];
        while remaining > 0 {
            anyhow::ensure!(Instant::now() < deadline, "snapshot verification timed out");
            let amount = usize::try_from(remaining.min(buffer.len() as u64))?;
            file.read_exact(&mut buffer[..amount])?;
            digest.update(&buffer[..amount]);
            remaining -= amount as u64;
        }
        self.revalidate()?;
        Ok(digest.finalize().into())
    }

    fn completed_authority(
        &mut self,
        source_connection: &turso::Connection,
        source: &Self,
    ) -> anyhow::Result<bool> {
        let connection = self.connect()?;
        if self.verify(&connection).is_err() {
            return Ok(false);
        }
        let record = required(source_connection)?;
        if !record.fenced {
            return Ok(false);
        }
        let handoff = record
            .handoff
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("source fence is incomplete"))?;
        self.activate(
            &connection,
            source,
            source_connection,
            record.authority.generation,
            handoff,
        )?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_resumes_only_the_registered_partial_leaf_after_restart() {
        let root = std::env::temp_dir().join(format!("keeppeek-snapshot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let source_path = root.join("source.db");
        let target_path = root.join("target.db");
        let mut source = Lease::acquire(&source_path).unwrap();
        let connection = source.connect().unwrap();
        let owner = source.initialize(&connection).unwrap();
        let mut target = Lease::acquire(&target_path).unwrap();
        source
            .fence(
                &connection,
                owner.generation,
                &uuid::Uuid::new_v4().to_string(),
                &target,
            )
            .unwrap();
        pollster::block_on(connection.execute(SCHEMA, ())).unwrap();
        source.register_snapshot(&connection, &mut target).unwrap();
        std::fs::write(&target_path, b"interrupted VACUUM output").unwrap();
        std::fs::write(target.scratch_path().join("image"), b"partial snapshot").unwrap();
        drop(connection);
        drop(target);
        drop(source);
        let mut source = Lease::acquire(&source_path).unwrap();
        let connection = source.connect().unwrap();
        let mut target = Lease::acquire(&target_path).unwrap();
        source
            .snapshot_into(&connection, &mut target, 8 * 1024 * 1024)
            .unwrap();
        let copied = target.connect().unwrap();
        crate::backup::database::validate_connection(
            &copied,
            crate::backup::BackupSection::RecordingCatalog,
            false,
        )
        .unwrap();
        assert!(source.verify(&connection).is_err());
    }

    #[test]
    fn snapshot_refuses_a_replacement_for_the_registered_partial_leaf() {
        let root = std::env::temp_dir().join(format!("keeppeek-snapshot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let mut source = Lease::acquire(&root.join("source.db")).unwrap();
        let connection = source.connect().unwrap();
        source.initialize(&connection).unwrap();
        let target_path = root.join("target.db");
        let mut target = Lease::acquire(&target_path).unwrap();
        pollster::block_on(connection.execute(SCHEMA, ())).unwrap();
        source.register_snapshot(&connection, &mut target).unwrap();
        drop(target);
        std::fs::rename(&target_path, root.join("captured.db")).unwrap();
        std::fs::write(&target_path, b"unrelated file").unwrap();
        let mut target = Lease::acquire(&target_path).unwrap();
        assert!(
            source
                .snapshot_into(&connection, &mut target, 8 * 1024 * 1024)
                .is_err()
        );
        assert_eq!(std::fs::read(&target_path).unwrap(), b"unrelated file");
    }
}
