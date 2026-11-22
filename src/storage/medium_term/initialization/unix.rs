//! A staged entry retains its original leaf name in the same recording hour directory.
//! This gives interrupted or conflicting restores a durable, non-overwriting recovery path.
use crate::storage::long_term::inspection::removal::{
    private_directory, sync_directory, unix::rename_to, validate_owner,
};
use cap_fs_ext::{
    DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt, OpenOptionsSyncExt,
};
use cap_std::fs::{Dir, File, Metadata, MetadataExt, OpenOptions};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Read},
};

pub(super) const RECOVERY_DIRECTORY: &str = ".keeppeek-header-recovery-v1";

const INTENT_MAGIC: &[u8; 8] = b"KPHDRv1\0";

fn intent_name(name: &OsStr) -> OsString {
    let mut marker = name.to_owned();
    marker.push(".origin");
    marker
}

fn pin_name(name: &OsStr) -> OsString {
    let mut pin = name.to_owned();
    pin.push(".pin");
    pin
}

fn entry(directory: &Dir, name: &OsStr) -> io::Result<Option<Metadata>> {
    match directory.symlink_metadata(name) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn matches_record(metadata: &Metadata, record: &[u8; 24]) -> bool {
    metadata.is_file()
        && record[8..16] == metadata.dev().to_be_bytes()
        && record[16..] == metadata.ino().to_be_bytes()
}

fn read_legacy_record(stage: &Dir, name: &OsStr) -> io::Result<[u8; 24]> {
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

fn pins_name(name: &OsStr) -> OsString {
    let mut directory = name.to_owned();
    directory.push(".pins-v2");
    directory
}

fn pin_leaf(metadata: &Metadata) -> String {
    format!("pin-{:016x}-{:016x}", metadata.dev(), metadata.ino())
}

fn record_for(metadata: &Metadata) -> [u8; 24] {
    let mut record = [0; 24];
    record[..8].copy_from_slice(INTENT_MAGIC);
    record[8..16].copy_from_slice(&metadata.dev().to_be_bytes());
    record[16..].copy_from_slice(&metadata.ino().to_be_bytes());
    record
}

enum Pin {
    Legacy(bool),
    Named { directory: Dir, leaf: OsString },
}

struct Intent {
    record: [u8; 24],
    pin: Pin,
}

fn read_intent(stage: &Dir, name: &OsStr) -> io::Result<Intent> {
    if entry(stage, &pins_name(name))?.is_some() {
        let directory = private_directory(stage, &pins_name(name), false)?;
        let mut entries = directory.entries()?;
        if let Some(first) = entries.next().transpose()? {
            let leaf = first.file_name();
            let metadata = directory.symlink_metadata(&leaf)?;
            if entries.next().transpose()?.is_some()
                || !metadata.is_file()
                || leaf != OsStr::new(&pin_leaf(&metadata))
            {
                return Err(io::Error::other("unrecognized recording recovery pin"));
            }
            drop(entries);
            return Ok(Intent {
                record: record_for(&metadata),
                pin: Pin::Named { directory, leaf },
            });
        }
    }
    let record = read_legacy_record(stage, name)?;
    let pin = entry(stage, &pin_name(name))?;
    if pin
        .as_ref()
        .is_some_and(|metadata| !matches_record(metadata, &record))
    {
        return Err(io::Error::other("recording recovery pin changed"));
    }
    Ok(Intent {
        record,
        pin: Pin::Legacy(pin.is_some()),
    })
}

fn create_intent(parent: &Dir, stage: &Dir, name: &OsStr, owned: &Metadata) -> io::Result<()> {
    if entry(stage, name)?.is_some()
        || entry(stage, &intent_name(name))?.is_some()
        || entry(stage, &pin_name(name))?.is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a prior staged recording entry needs recovery",
        ));
    }
    let directory = private_directory(stage, &pins_name(name), true)?;
    if directory.entries()?.next().transpose()?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a prior staged recording entry needs recovery",
        ));
    }
    // The link name records the identity from the caller's open handle. An interrupted
    // create therefore leaves a complete intent or no pin, never a pin awaiting a marker.
    let leaf = pin_leaf(owned);
    parent.hard_link(name, &directory, &leaf)?;
    if !matches_record(&directory.symlink_metadata(&leaf)?, &record_for(owned)) {
        directory.remove_file(&leaf)?;
        sync_directory(&directory)?;
        return Err(io::Error::other("active recording changed before cleanup"));
    }
    sync_directory(&directory)?;
    sync_directory(stage)
}

impl Intent {
    fn finish(&self, stage: &Dir, name: &OsStr) -> io::Result<()> {
        match &self.pin {
            Pin::Named { directory, leaf } => {
                if !matches_record(&directory.symlink_metadata(leaf)?, &self.record) {
                    return Err(io::Error::other("recording recovery pin changed"));
                }
                directory.remove_file(leaf)?;
                sync_directory(directory)?;
                stage.remove_dir(pins_name(name))?;
            }
            Pin::Legacy(pinned) => {
                if *pinned {
                    let pin = pin_name(name);
                    if !matches_record(&stage.symlink_metadata(&pin)?, &self.record) {
                        return Err(io::Error::other("recording recovery pin changed"));
                    }
                    stage.remove_file(pin)?;
                }
                stage.remove_file(intent_name(name))?;
            }
        }
        sync_directory(stage)
    }
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
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::NotADirectory
                    | io::ErrorKind::PermissionDenied
            ) || error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if let Err(error) = validate_owner(&stage, 0o077) {
        // Never inspect or mutate an untrusted directory. legacy() still creates the active
        // file exclusively, so skipping replay cannot overwrite a recording.
        return if error.kind() == io::ErrorKind::PermissionDenied {
            Ok(())
        } else {
            Err(error)
        };
    }
    if entry(&stage, &intent_name(name))?.is_none()
        && entry(&stage, &pins_name(name))?.is_none()
        && entry(&stage, name)?.is_none()
    {
        return Ok(());
    }
    let _lease = lock_stage(&stage)?;
    let staged = entry(&stage, name)?;
    let intent = match read_intent(&stage, name) {
        Ok(intent) => intent,
        Err(error) if staged.is_none() && error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if staged.is_none() {
        // A legacy marker without a pin cannot prove that a public inode wasn't recycled.
        if matches!(intent.pin, Pin::Legacy(false))
            || !entry(parent, name)?
                .is_some_and(|metadata| matches_record(&metadata, &intent.record))
        {
            return intent.finish(&stage, name);
        }
        rename_to(parent, name, &stage, name)?;
        sync_directory(&stage)?;
        sync_directory(parent)?;
    }
    reconcile_staged(&stage, parent, name, &intent)
}

fn reconcile_staged(stage: &Dir, parent: &Dir, name: &OsStr, intent: &Intent) -> io::Result<()> {
    // Recheck after the no-replace move: a different file may have taken the public name.
    if matches_record(&stage.symlink_metadata(name)?, &intent.record) {
        stage.remove_file(name)?;
    } else {
        rename_to(stage, name, parent, name).map_err(|error| {
            io::Error::new(error.kind(), format!(
                "active recording name changed; displaced entry retained under {RECOVERY_DIRECTORY} with its original name: {error}"
            ))
        })?;
        sync_directory(parent)?;
    }
    sync_directory(stage)?;
    intent.finish(stage, name)
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
    create_intent(parent, &stage, name, &owned)?;
    before_stage()?;
    rename_to(parent, name, &stage, name)?;
    sync_directory(&stage)?;
    sync_directory(parent)?;
    after_stage()?;
    let intent = read_intent(&stage, name)?;
    reconcile_staged(&stage, parent, name, &intent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
        stage.remove_file(name)?;
        parent.write(name, b"new partial")?;
        let pin = pin_name(name);
        stage.write(&pin, b"older pin")?;
        let error =
            create_intent(&parent, &stage, name, &parent.symlink_metadata(name)?).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(error.to_string().contains("needs recovery"));
        assert_eq!(stage.read(&pin)?, b"older pin");
        assert_eq!(parent.read(name)?, b"new partial");
        drop(stage);
        drop(parent);
        std::fs::remove_dir_all(root)
    }

    #[test]
    fn a_synced_pin_recovers_without_a_marker_and_rejects_an_unrelated_inode() -> io::Result<()> {
        let root =
            std::env::temp_dir().join(format!("keeppeek-header-pin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        let parent = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        let stage = private_directory(&parent, OsStr::new(RECOVERY_DIRECTORY), true)?;
        let name = OsStr::new("recording.mp4.active");
        parent.write(name, b"partial")?;
        create_intent(&parent, &stage, name, &parent.symlink_metadata(name)?)?;
        assert!(entry(&stage, &intent_name(name))?.is_none());
        drop(stage);
        drop(parent);
        super::super::legacy(&root.join(name), |mut fresh| fresh.write_all(b"new header"))?;
        let parent = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        assert_eq!(parent.read(name)?, b"new header");
        let stage = private_directory(&parent, OsStr::new(RECOVERY_DIRECTORY), false)?;
        let pins = private_directory(&stage, &pins_name(name), true)?;
        let expected = pin_leaf(&parent.symlink_metadata(name)?);
        pins.write(&expected, b"unrelated")?;
        assert!(restore_pending(&parent, name).is_err());
        assert_eq!(pins.read(&expected)?, b"unrelated");
        assert_eq!(parent.read(name)?, b"new header");
        drop(pins);
        drop(stage);
        drop(parent);
        std::fs::remove_dir_all(root)
    }

    #[test]
    fn legacy_origin_requires_its_original_pin_before_reclaiming_an_active_name() -> io::Result<()>
    {
        let root =
            std::env::temp_dir().join(format!("keeppeek-header-v1-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        let parent = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        let stage = private_directory(&parent, OsStr::new(RECOVERY_DIRECTORY), true)?;
        let name = OsStr::new("recording.mp4.active");
        parent.write(name, b"retained")?;
        let put_origin = || {
            stage.write(
                intent_name(name),
                record_for(&parent.symlink_metadata(name)?),
            )
        };
        put_origin()?;
        restore_pending(&parent, name)?;
        assert_eq!(parent.read(name)?, b"retained");
        put_origin()?;
        parent.hard_link(name, &stage, pin_name(name))?;
        restore_pending(&parent, name)?;
        assert!(!parent.exists(name));
        assert_eq!(stage.entries()?.count(), 0);
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
        create_intent(&parent, &stage, name, &owned)?;
        let lease = lock_stage(&stage)?;
        assert_eq!(
            restore_pending(&parent, name).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(lease);
        super::super::legacy(&root.join(name), |mut fresh| fresh.write_all(b"new header"))?;
        assert_eq!(parent.read(name)?, b"new header");
        assert_eq!(stage.entries()?.count(), 0);

        let owned = parent.symlink_metadata(name)?;
        create_intent(&parent, &stage, name, &owned)?;
        parent.remove_file(name)?;
        parent.write(name, b"replacement before move")?;
        assert_eq!(
            super::super::legacy(&root.join(name), |_| Ok(()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(parent.read(name)?, b"replacement before move");
        assert_eq!(stage.entries()?.count(), 0);

        let owned = parent.symlink_metadata(name)?;
        create_intent(&parent, &stage, name, &owned)?;
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
