//! A staged entry retains its original leaf name in the same recording hour directory.
//! This gives interrupted or conflicting restores a durable, non-overwriting recovery path.
use crate::storage::long_term::inspection::removal::{
    private_directory, sync_directory, unix::rename_to, validate_owner,
};
use cap_fs_ext::{
    DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt, OpenOptionsSyncExt,
};
use cap_std::fs::{Dir, File, MetadataExt, OpenOptions};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
};

pub(super) const RECOVERY_DIRECTORY: &str = ".keeppeek-header-recovery-v1";

const INTENT_MAGIC: &[u8; 8] = b"KPHDRv1\0";

fn intent_name(name: &OsStr) -> OsString {
    let mut marker = name.to_owned();
    marker.push(".origin");
    marker
}

fn read_intent(stage: &Dir, name: &OsStr) -> io::Result<[u8; 24]> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut intent = stage.open_with(intent_name(name), &options)?;
    let metadata = intent.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() != 24 {
        return Err(io::Error::other(
            "invalid recording initialization recovery record",
        ));
    }
    let mut record = [0; 24];
    intent.read_exact(&mut record)?;
    if &record[..8] != INTENT_MAGIC {
        return Err(io::Error::other(
            "invalid recording initialization recovery marker",
        ));
    }
    Ok(record)
}

fn create_intent(stage: &Dir, name: &OsStr, owned: &cap_std::fs::Metadata) -> io::Result<()> {
    match stage.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a prior staged recording entry needs recovery",
            ));
        }
        Err(error) => return Err(error),
    }
    let mut marker = stage.open_with(
        intent_name(name),
        OpenOptions::new().create_new(true).write(true),
    )?;
    let mut record = [0; 24];
    record[..8].copy_from_slice(INTENT_MAGIC);
    record[8..16].copy_from_slice(&owned.dev().to_be_bytes());
    record[16..].copy_from_slice(&owned.ino().to_be_bytes());
    marker.write_all(&record)?;
    marker.sync_all()?;
    sync_directory(stage)
}

fn finish(stage: &Dir, name: &OsStr) -> io::Result<()> {
    read_intent(stage, name)?;
    stage.remove_file(intent_name(name))?;
    sync_directory(stage)
}

// Serialize live cleanup and crash replay across processes. An open directory description
// holds the lease without leaving a removable lock file in the recovery namespace.
fn lock_stage(stage: &Dir) -> io::Result<std::fs::File> {
    let lease = stage
        .open_with(".", OpenOptions::new().read(true).maybe_dir(true))?
        .into_std();
    lease.try_lock().map_err(io::Error::from)?;
    Ok(lease)
}

pub(super) fn restore_pending(parent: &Dir, name: &OsStr) -> io::Result<()> {
    let stage = match parent.open_dir_nofollow(RECOVERY_DIRECTORY) {
        Ok(stage) => stage,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    validate_owner(&stage, 0o077)?;
    let _lease = lock_stage(&stage)?;
    let entry = match stage.symlink_metadata(name) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let record = match read_intent(&stage, name) {
        Ok(record) => record,
        Err(error) if entry.is_none() && error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match entry {
        Some(metadata)
            if metadata.is_file()
                && record[8..16] == metadata.dev().to_be_bytes()
                && record[16..] == metadata.ino().to_be_bytes() =>
        {
            stage.remove_file(name)?;
            sync_directory(&stage)?;
            finish(&stage, name)
        }
        Some(_) => restore(&stage, parent, name),
        // A crash either preceded the move or followed a successful removal/restoration.
        None => finish(&stage, name),
    }
}

fn restore(stage: &Dir, parent: &Dir, name: &OsStr) -> io::Result<()> {
    rename_to(stage, name, parent, name)?;
    sync_directory(parent)?;
    sync_directory(stage)?;
    finish(stage, name)
}

pub(super) fn discard(parent: &Dir, name: &OsStr, file: File) -> io::Result<()> {
    discard_with(parent, name, file, || Ok(()), || Ok(()))
}

fn discard_with(
    parent: &Dir,
    name: &OsStr,
    file: File,
    before_stage: impl FnOnce() -> io::Result<()>,
    after_stage: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let owned = file.metadata()?;
    let matches = |entry: &cap_std::fs::Metadata| {
        entry.is_file() && entry.dev() == owned.dev() && entry.ino() == owned.ino()
    };
    match parent.symlink_metadata(name) {
        Ok(entry) if matches(&entry) => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }

    // The durable marker records the pinned identity and original name before the move.
    // Neither staging nor restoration can overwrite an occupant.
    let stage = private_directory(parent, OsStr::new(RECOVERY_DIRECTORY), true)?;
    let _lease = lock_stage(&stage)?;
    create_intent(&stage, name, &owned)?;
    if let Err(error) = before_stage().and_then(|()| rename_to(parent, name, &stage, name)) {
        finish(&stage, name)?;
        return Err(error);
    }
    sync_directory(&stage)?;
    sync_directory(parent)?;
    after_stage()?;
    if !matches(&stage.symlink_metadata(name)?) {
        return restore(&stage, parent, name).map_err(|error| {
            io::Error::new(error.kind(), format!(
                "active recording name changed; displaced entry retained under {RECOVERY_DIRECTORY} with its original name: {error}"
            ))
        });
    }
    stage.remove_file(name)?;
    sync_directory(&stage)?;
    finish(&stage, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmarked_private_file_is_never_restored_as_an_active_recording() -> io::Result<()> {
        let root =
            std::env::temp_dir().join(format!("keeppeek-header-unowned-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        let parent = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        let stage = private_directory(&parent, OsStr::new(RECOVERY_DIRECTORY), true)?;
        let name = OsStr::new("recording.mp4.active");
        stage.write(name, b"unrelated")?;
        assert!(restore_pending(&parent, name).is_err());
        assert_eq!(stage.read(name)?, b"unrelated");
        assert!(!parent.exists(name));
        drop(stage);
        drop(parent);
        std::fs::remove_dir_all(root)
    }

    #[test]
    fn restart_finishes_owned_partial_cleanup_and_clears_the_interrupted_intent() -> io::Result<()>
    {
        let root =
            std::env::temp_dir().join(format!("keeppeek-header-replay-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        let parent = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        let stage = private_directory(&parent, OsStr::new(RECOVERY_DIRECTORY), true)?;
        let name = OsStr::new("recording.mp4.active");
        parent.write(name, b"partial")?;
        let owned = parent.symlink_metadata(name)?;
        create_intent(&stage, name, &owned)?;
        let lease = lock_stage(&stage)?;
        assert_eq!(
            restore_pending(&parent, name).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(lease);
        restore_pending(&parent, name)?;
        assert_eq!(parent.read(name)?, b"partial");
        assert_eq!(stage.entries()?.count(), 0);

        create_intent(&stage, name, &owned)?;
        rename_to(&parent, name, &stage, name)?;
        parent.write(name, b"current occupant")?;
        restore_pending(&parent, name)?;
        assert_eq!(parent.read(name)?, b"current occupant");
        assert_eq!(stage.entries()?.count(), 0);
        drop(stage);
        drop(parent);
        std::fs::remove_dir_all(root)
    }

    #[test]
    fn occupied_restore_durably_preserves_both_replacements_and_recovers_the_original_name()
    -> io::Result<()> {
        let root =
            std::env::temp_dir().join(format!("keeppeek-header-race-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        let parent = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        let name = OsStr::new("recording.mp4.active");
        let pinned = parent.open_with(name, OpenOptions::new().write(true).create_new(true))?;
        let result = discard_with(
            &parent,
            name,
            pinned,
            || {
                parent.rename(name, &parent, "writer-owned")?;
                parent.write(name, b"first replacement")
            },
            || parent.write(name, b"current occupant"),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(parent.read(name)?, b"current occupant");
        drop(parent);
        let reopened = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        let staged = reopened.open_dir_nofollow(RECOVERY_DIRECTORY)?;
        assert_eq!(staged.read(name)?, b"first replacement");
        assert_eq!(
            restore_pending(&reopened, name).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(reopened.read(name)?, b"current occupant");
        reopened.rename(name, &reopened, "kept-occupant")?;
        restore_pending(&reopened, name)?;
        assert_eq!(reopened.read(name)?, b"first replacement");
        assert_eq!(reopened.read("kept-occupant")?, b"current occupant");
        assert_eq!(reopened.read("writer-owned")?, b"");
        assert_eq!(staged.entries()?.count(), 0);
        drop(staged);
        drop(reopened);
        std::fs::remove_dir_all(root)
    }
}
