//! Confined retirement receipts survive uncertain catalog acknowledgements.

use super::{
    Root,
    file::{file_identity, file_options, hash, validate_key},
    identity,
};
use crate::storage::long_term::inspection::removal::{
    self, private_directory, sync_directory, validate_owner,
};
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, Write},
    time::{Duration, Instant},
};

const LOCK: &str = "lock";
const QUARANTINE: &str = ".retired";
const LEAF: &str = "recording.mp4";
const INTENT: &str = "intent.json";
const STAGED: &str = "staged.json";
const MAX_RECEIPTS: usize = 4096;
const MAX_RECEIPT_BYTES: u64 = 4096;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Receipt {
    operation: String,
    key: String,
    file: String,
    bytes: u64,
    digest: [u8; 32],
    filesystem: String,
    root: String,
    quarantine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding: Option<LegacyBinding>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LegacyBinding {
    filesystem: String,
    root: String,
    parent: String,
}

struct Retirement<'a> {
    root: &'a Root,
    binding: Option<&'a Root>,
    lock: File,
    staging: Dir,
    directory: Dir,
    receipt: Receipt,
    deadline: Instant,
}

impl Root {
    /// Confirms absence of at most two confined leaves under the pinned root.
    /// This observation alone never proves retirement of a previously owned file.
    ///
    /// # Errors
    /// Rejects present entries, unsafe keys, changed roots, or failed synchronization.
    pub fn confirm_absent(&self, keys: &[&str]) -> anyhow::Result<()> {
        anyhow::ensure!(
            !keys.is_empty() && keys.len() <= 2,
            "invalid absence check size"
        );
        self.sync()?;
        for key in keys {
            validate_key(key)?;
            anyhow::ensure!(absent(&self.directory, key)?, "owned leaf is still present");
        }
        self.sync()
    }

    /// Retires an exact owned leaf through a private, durable quarantine receipt.
    /// The caller must hold the move worker lease and authorize source retirement.
    /// Receipts remain until catalog acknowledgement; 4096 retained jobs stop new work.
    ///
    /// # Errors
    /// Rejects ambiguous absence, changed evidence, unsafe files, torn receipts, and exhausted bounds.
    pub fn retire_owned(
        &self,
        key: &str,
        expected_identity: &str,
        bytes: u64,
        digest: [u8; 32],
        operation: &str,
    ) -> anyhow::Result<()> {
        Retirement::prepare(self, key, expected_identity, bytes, digest, operation)?.finish()
    }

    pub(super) fn retire_with_binding(
        &self,
        key: &str,
        expected_identity: &str,
        bytes: u64,
        digest: [u8; 32],
        operation: &str,
        binding: Option<&Self>,
    ) -> anyhow::Result<()> {
        Retirement::prepare_with_binding(
            self,
            key,
            expected_identity,
            bytes,
            digest,
            operation,
            binding,
        )?
        .finish()
    }

    /// Removes retirement receipts only after the catalog has durably completed this job.
    /// A pinned receipt moves outside the job directory before its metadata is removed.
    /// This operation never removes a media file.
    ///
    /// # Errors
    /// Rejects changed receipts, ambiguous entries, replaced directories, or unsafe roots.
    pub fn acknowledge_retirement(
        &self,
        key: &str,
        expected_identity: &str,
        bytes: u64,
        digest: [u8; 32],
        operation: &str,
    ) -> anyhow::Result<()> {
        self.acknowledge_with_binding(key, expected_identity, bytes, digest, operation, None)
    }

    pub(super) fn acknowledge_with_binding(
        &self,
        key: &str,
        expected_identity: &str,
        bytes: u64,
        digest: [u8; 32],
        operation: &str,
        binding: Option<&Self>,
    ) -> anyhow::Result<()> {
        validate_retirement_key(key, binding)?;
        validate_binding(binding)?;
        anyhow::ensure!(
            operation.len() == 36 && uuid::Uuid::parse_str(operation)?.to_string() == operation,
            "invalid retirement operation"
        );
        self.sync()?;
        let staging = match private_directory(&self.directory, OsStr::new(QUARANTINE), false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let _lock = lock_staging(&staging)?;
        let ack = format!("{operation}.ack");
        let directory = match private_directory(&staging, OsStr::new(operation), false) {
            Ok(directory) => Some(directory),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut receipt = read_receipt(&staging, &ack)?;
        if receipt.is_none() {
            let Some(directory) = &directory else {
                return Ok(());
            };
            let staged = read_receipt(directory, STAGED)?
                .ok_or_else(|| anyhow::anyhow!("retirement acknowledgement proof is missing"))?;
            validate_receipt(
                self,
                &staged,
                key,
                expected_identity,
                bytes,
                digest,
                operation,
            )?;
            validate_receipt_binding(self, binding, &staged)?;
            validate_ack(self, directory, &staged)?;
            validate_intent(directory, &staged)?;
            let file = open_removal(directory, STAGED)?;
            rename_receipt(directory, &staging, &ack, &file)?;
            sync_directory(directory)?;
            sync_directory(&staging)?;
            receipt = Some(staged);
        }
        let receipt = receipt.expect("receipt loaded or moved");
        validate_receipt(
            self,
            &receipt,
            key,
            expected_identity,
            bytes,
            digest,
            operation,
        )?;
        validate_receipt_binding(self, binding, &receipt)?;
        finish_ack(self, &staging, directory, &receipt, &ack, binding)
    }
}

impl<'a> Retirement<'a> {
    fn prepare(
        root: &'a Root,
        key: &str,
        expected_identity: &str,
        bytes: u64,
        digest: [u8; 32],
        operation: &str,
    ) -> anyhow::Result<Self> {
        Self::prepare_with_binding(root, key, expected_identity, bytes, digest, operation, None)
    }

    fn prepare_with_binding(
        root: &'a Root,
        key: &str,
        expected_identity: &str,
        bytes: u64,
        digest: [u8; 32],
        operation: &str,
        binding: Option<&'a Root>,
    ) -> anyhow::Result<Self> {
        validate_retirement_key(key, binding)?;
        validate_binding(binding)?;
        anyhow::ensure!(
            operation.len() == 36 && uuid::Uuid::parse_str(operation)?.to_string() == operation,
            "invalid retirement operation"
        );
        anyhow::ensure!(
            !expected_identity.is_empty() && expected_identity.len() <= 256,
            "invalid retirement evidence"
        );
        root.sync()?;
        let staging = private_directory(&root.directory, OsStr::new(QUARANTINE), true)?;
        let lock = lock_staging(&staging)?;
        bounded_directory(&staging, operation)?;
        let directory = private_directory(&staging, OsStr::new(operation), true)?;
        let observed = identity(&directory)?;
        anyhow::ensure!(
            observed.filesystem == root.identity.filesystem,
            "retirement changed filesystem"
        );
        let receipt = Receipt {
            operation: operation.into(),
            key: key.into(),
            file: expected_identity.into(),
            bytes,
            digest,
            filesystem: root.identity.filesystem.clone(),
            root: root.identity.directory.clone(),
            quarantine: observed.directory,
            binding: receipt_binding(root, binding)?,
        };
        let retirement = Self {
            root,
            binding,
            lock,
            staging,
            directory,
            receipt,
            deadline: Instant::now() + Duration::from_secs(300),
        };
        retirement.revalidate()?;
        retirement.prepare_intent()?;
        Ok(retirement)
    }

    fn prepare_intent(&self) -> anyhow::Result<()> {
        match read_receipt(&self.directory, INTENT)? {
            Some(existing) => {
                anyhow::ensure!(existing == self.receipt, "retirement intent changed");
            }
            None => {
                anyhow::ensure!(
                    absent(&self.directory, LEAF)? && absent(&self.directory, STAGED)?,
                    "retirement quarantine has no ownership receipt"
                );
                let mut source = open_removal(&self.root.directory, &self.receipt.key)?;
                self.verify(&mut source)?;
                self.revalidate()?;
                write_receipt(&self.directory, INTENT, &self.receipt)?;
            }
        }
        Ok(())
    }

    fn finish(&self) -> anyhow::Result<()> {
        self.revalidate()?;
        let completed_staging = read_receipt(&self.directory, STAGED)?;
        if let Some(receipt) = &completed_staging {
            anyhow::ensure!(
                *receipt == self.receipt,
                "retirement staging receipt changed"
            );
        }
        if absent(&self.directory, LEAF)? {
            if completed_staging.is_some() {
                anyhow::ensure!(
                    absent(&self.root.directory, &self.receipt.key)?,
                    "retirement source reappeared"
                );
                self.sync()?;
                return Ok(());
            }
            let mut source = open_removal(&self.root.directory, &self.receipt.key)?;
            self.verify(&mut source)?;
            self.revalidate()?;
            verify_named(&self.root.directory, &self.receipt.key, &self.receipt.file)?;
            rename(
                &self.root.directory,
                &self.receipt.key,
                &self.directory,
                &source,
            )?;
            self.sync()?;
        }
        anyhow::ensure!(
            absent(&self.root.directory, &self.receipt.key)?,
            "retirement source reappeared"
        );
        let mut staged = open_removal(&self.directory, LEAF)?;
        self.verify(&mut staged)?;
        self.revalidate()?;
        verify_named(&self.directory, LEAF, &self.receipt.file)?;
        if completed_staging.is_none() {
            self.sync()?;
            write_receipt(&self.directory, STAGED, &self.receipt)?;
        }
        self.revalidate()?;
        verify_named(&self.directory, LEAF, &self.receipt.file)?;
        remove(&self.directory, staged)?;
        anyhow::ensure!(
            absent(&self.directory, LEAF)? && absent(&self.root.directory, &self.receipt.key)?,
            "retirement namespace changed"
        );
        self.sync()
    }

    fn verify(&self, file: &mut File) -> anyhow::Result<()> {
        anyhow::ensure!(Instant::now() < self.deadline, "retirement timed out");
        anyhow::ensure!(
            file_identity(file)? == self.receipt.file
                && file.metadata()?.len() == self.receipt.bytes,
            "retirement file changed"
        );
        file.rewind()?;
        anyhow::ensure!(
            hash(file, self.receipt.bytes, self.deadline)? == self.receipt.digest,
            "retirement digest changed"
        );
        anyhow::ensure!(
            file_identity(file)? == self.receipt.file
                && file.metadata()?.len() == self.receipt.bytes,
            "retirement file changed"
        );
        Ok(())
    }

    fn revalidate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(Instant::now() < self.deadline, "retirement timed out");
        validate_binding(self.binding)?;
        self.root.revalidate()?;
        validate_owner(&self.root.directory, 0o022)?;
        let staging = private_directory(&self.root.directory, OsStr::new(QUARANTINE), false)?;
        verify_named(&staging, LOCK, &file_identity(&self.lock)?)?;
        anyhow::ensure!(
            identity(&staging)? == identity(&self.staging)?,
            "retirement staging changed"
        );
        let directory = private_directory(&staging, OsStr::new(&self.receipt.operation), false)?;
        let expected = identity(&self.directory)?;
        anyhow::ensure!(
            identity(&directory)? == expected
                && expected.directory == self.receipt.quarantine
                && expected.filesystem == self.receipt.filesystem,
            "retirement directory changed"
        );
        let mut entries = directory.entries()?;
        for _ in 0..4 {
            let Some(entry) = entries.next().transpose()? else {
                return Ok(());
            };
            anyhow::ensure!(
                [INTENT, STAGED, LEAF]
                    .iter()
                    .any(|name| entry.file_name() == *name),
                "unexpected retirement entry"
            );
        }
        anyhow::bail!("retirement directory entry limit exceeded")
    }

    fn sync(&self) -> anyhow::Result<()> {
        sync_directory(&self.root.directory)?;
        sync_directory(&self.directory)?;
        self.revalidate()
    }
}

fn bounded_directory(directory: &Dir, operation: &str) -> anyhow::Result<()> {
    let mut count = 0;
    let mut existing = false;
    for (index, entry) in directory.entries()?.take(2 * MAX_RECEIPTS + 2).enumerate() {
        anyhow::ensure!(
            index <= 2 * MAX_RECEIPTS,
            "retirement receipt entry limit exceeded"
        );
        let entry = entry?;
        if entry.file_name() == LOCK
            || entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".ack"))
        {
            continue;
        }
        count += 1;
        existing |= entry.file_name() == operation;
        anyhow::ensure!(count <= MAX_RECEIPTS, "retirement receipt limit exceeded");
    }
    anyhow::ensure!(
        existing || count < MAX_RECEIPTS,
        "retirement receipt limit exceeded"
    );
    Ok(())
}

fn absent(directory: &Dir, name: &str) -> anyhow::Result<bool> {
    match directory.symlink_metadata(name) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn read_receipt(directory: &Dir, name: &str) -> anyhow::Result<Option<Receipt>> {
    if absent(directory, name)? {
        return Ok(None);
    }
    let file = directory.open_with(name, &file_options())?.into_std();
    let identity = file_identity(&file)?;
    anyhow::ensure!(
        file.metadata()?.len() <= MAX_RECEIPT_BYTES,
        "retirement receipt is too large"
    );
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= usize::try_from(MAX_RECEIPT_BYTES)?,
        "retirement receipt is too large"
    );
    verify_named(directory, name, &identity)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn write_receipt(directory: &Dir, name: &str, receipt: &Receipt) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(receipt)?;
    anyhow::ensure!(
        bytes.len() <= usize::try_from(MAX_RECEIPT_BYTES)?,
        "retirement receipt is too large"
    );
    let mut options = file_options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = directory.open_with(name, &options)?.into_std();
    let identity = file_identity(&file)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    verify_named(directory, name, &identity)?;
    sync_directory(directory)?;
    Ok(())
}

fn verify_named(directory: &Dir, name: &str, expected: &str) -> anyhow::Result<()> {
    let file = directory.open_with(name, &file_options())?.into_std();
    anyhow::ensure!(file_identity(&file)? == expected, "retirement leaf changed");
    Ok(())
}

fn open_removal(directory: &Dir, name: &str) -> anyhow::Result<File> {
    #[cfg(windows)]
    {
        Ok(removal::windows::exclusive_file(directory, OsStr::new(name))?.into_std())
    }
    #[cfg(not(windows))]
    {
        Ok(directory.open_with(name, &file_options())?.into_std())
    }
}

fn rename(parent: &Dir, key: &str, directory: &Dir, file: &File) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        removal::windows::rename_to(file, directory, OsStr::new(LEAF))?;
    }
    #[cfg(unix)]
    {
        let _ = file;
        removal::unix::rename_to(parent, OsStr::new(key), directory, OsStr::new(LEAF))?;
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (parent, key, directory, file);
        anyhow::bail!("retirement is unsupported");
    }
    #[cfg(windows)]
    let _ = (parent, key);
    Ok(())
}

fn remove(directory: &Dir, file: File) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let _ = directory;
        removal::windows::remove(cap_std::fs::File::from_std(file))?;
    }
    #[cfg(unix)]
    {
        use cap_fs_ext::MetadataExt;
        directory.remove_file(LEAF)?;
        anyhow::ensure!(
            cap_std::fs::File::from_std(file).metadata()?.nlink() == 0,
            "retirement unlink did not remove selected file"
        );
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (directory, file);
        anyhow::bail!("retirement is unsupported");
    }
    Ok(())
}

#[cfg(test)]
mod tests;

fn lock_staging(directory: &Dir) -> anyhow::Result<File> {
    let mut options = file_options();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = directory.open_with(LOCK, &options)?.into_std();
    let identity = file_identity(&lock)?;
    lock.try_lock()
        .map_err(|_| anyhow::anyhow!("volume retirement is already active"))?;
    verify_named(directory, LOCK, &identity)?;
    lock.sync_all()?;
    sync_directory(directory)?;
    Ok(lock)
}

fn validate_receipt(
    root: &Root,
    receipt: &Receipt,
    key: &str,
    expected_identity: &str,
    bytes: u64,
    digest: [u8; 32],
    operation: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        receipt.key == key
            && receipt.file == expected_identity
            && receipt.bytes == bytes
            && receipt.digest == digest
            && receipt.operation == operation
            && receipt.root == root.identity.directory
            && receipt.filesystem == root.identity.filesystem,
        "retirement acknowledgement intent changed"
    );
    Ok(())
}

fn validate_ack(root: &Root, directory: &Dir, receipt: &Receipt) -> anyhow::Result<()> {
    let observed = identity(directory)?;
    anyhow::ensure!(
        (&observed.directory, &observed.filesystem) == (&receipt.quarantine, &receipt.filesystem),
        "retirement directory changed"
    );
    anyhow::ensure!(
        absent(directory, LEAF)? && absent(&root.directory, &receipt.key)?,
        "retirement still contains media"
    );
    Ok(())
}

fn validate_intent(directory: &Dir, expected: &Receipt) -> anyhow::Result<()> {
    if let Some(intent) = read_receipt(directory, INTENT)? {
        anyhow::ensure!(intent == *expected, "retirement intent changed");
    }
    Ok(())
}

fn rename_receipt(source: &Dir, destination: &Dir, name: &str, file: &File) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let _ = source;
        removal::windows::rename_to(file, destination, OsStr::new(name))?;
    }
    #[cfg(unix)]
    {
        let _ = file;
        removal::unix::rename_to(source, OsStr::new(STAGED), destination, OsStr::new(name))?;
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (source, destination, name, file);
        anyhow::bail!("retirement is unsupported");
    }
    Ok(())
}

fn remove_receipt(directory: &Dir, name: &str, expected: &Receipt) -> anyhow::Result<()> {
    if absent(directory, name)? {
        return Ok(());
    }
    let file = open_removal(directory, name)?;
    let identity = file_identity(&file)?;
    anyhow::ensure!(
        file.metadata()?.len() <= MAX_RECEIPT_BYTES,
        "retirement receipt is too large"
    );
    let mut contents = Vec::new();
    file.try_clone()?
        .take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut contents)?;
    anyhow::ensure!(
        contents.len() <= usize::try_from(MAX_RECEIPT_BYTES)?
            && serde_json::from_slice::<Receipt>(&contents)? == *expected,
        "retirement acknowledgement receipt changed"
    );
    verify_named(directory, name, &identity)?;
    #[cfg(windows)]
    {
        removal::windows::remove(cap_std::fs::File::from_std(file))?;
    }
    #[cfg(unix)]
    {
        use cap_fs_ext::MetadataExt;
        directory.remove_file(name)?;
        anyhow::ensure!(
            cap_std::fs::File::from_std(file).metadata()?.nlink() == 0,
            "receipt unlink did not remove selected file"
        );
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = file;
        anyhow::bail!("retirement is unsupported");
    }
    Ok(())
}

fn validate_staging(root: &Root, staging: &Dir) -> anyhow::Result<()> {
    root.revalidate()?;
    let current = private_directory(&root.directory, OsStr::new(QUARANTINE), false)?;
    anyhow::ensure!(
        identity(&current)? == identity(staging)?,
        "retirement staging changed"
    );
    Ok(())
}

fn finish_ack(
    root: &Root,
    staging: &Dir,
    directory: Option<Dir>,
    receipt: &Receipt,
    ack: &str,
    binding: Option<&Root>,
) -> anyhow::Result<()> {
    if let Some(directory) = directory {
        validate_ack(root, &directory, receipt)?;
        anyhow::ensure!(
            absent(&directory, STAGED)?,
            "retirement has competing acknowledgement receipts"
        );
        validate_intent(&directory, receipt)?;
        validate_staging(root, staging)?;
        validate_binding(binding)?;
        remove_receipt(&directory, INTENT, receipt)?;
        sync_directory(&directory)?;
        root.revalidate()?;
        let expected = identity(&directory)?;
        drop(directory);
        let named = private_directory(staging, OsStr::new(&receipt.operation), false)?;
        anyhow::ensure!(
            identity(&named)? == expected,
            "retirement directory changed"
        );
        drop(named);
        validate_binding(binding)?;
        staging.remove_dir(&receipt.operation)?;
        sync_directory(staging)?;
    }
    validate_staging(root, staging)?;
    anyhow::ensure!(
        absent(staging, &receipt.operation)?,
        "retirement directory reappeared"
    );
    validate_binding(binding)?;
    remove_receipt(staging, ack, receipt)?;
    sync_directory(staging)?;
    root.sync()
}

fn validate_binding(binding: Option<&Root>) -> anyhow::Result<()> {
    if let Some(binding) = binding {
        binding.revalidate()?;
        validate_owner(&binding.directory, 0o022)?;
    }
    Ok(())
}

fn receipt_binding(parent: &Root, binding: Option<&Root>) -> anyhow::Result<Option<LegacyBinding>> {
    binding
        .map(|binding| {
            Ok(LegacyBinding {
                filesystem: binding.identity.filesystem.clone(),
                root: binding.identity.directory.clone(),
                parent: parent
                    .path
                    .strip_prefix(&binding.path)?
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid legacy parent path"))?
                    .replace('\\', "/"),
            })
        })
        .transpose()
}

fn validate_receipt_binding(
    parent: &Root,
    binding: Option<&Root>,
    receipt: &Receipt,
) -> anyhow::Result<()> {
    validate_binding(binding)?;
    anyhow::ensure!(
        receipt.binding == receipt_binding(parent, binding)?,
        "legacy retirement binding changed"
    );
    Ok(())
}

fn validate_retirement_key(key: &str, binding: Option<&Root>) -> anyhow::Result<()> {
    if binding.is_some() {
        anyhow::ensure!(
            super::legacy::components(key)?.len() == 1,
            "legacy retirement requires a leaf"
        );
        Ok(())
    } else {
        validate_key(key)
    }
}
