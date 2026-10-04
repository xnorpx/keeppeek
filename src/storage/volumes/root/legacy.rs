use super::{OwnedFile, Root, file::file_options, identity};
use crate::storage::long_term::inspection::removal::validate_owner;
use cap_fs_ext::DirExt;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

impl Root {
    /// Inspects a catalog-known legacy path without claiming ownership or changing bytes.
    /// The returned reader pins the binding, parent, identity, and observed length.
    ///
    /// # Errors
    /// Rejects unbounded paths, traversal, links, unsafe permissions, and replaced names.
    pub fn inspect_legacy(&self, relative_key: &str) -> anyhow::Result<OwnedFile> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (parent, key) = self.legacy_parent(relative_key)?;
        let binding = Self {
            path: self.path.clone(),
            directory: self.directory.try_clone()?,
            identity: self.identity.clone(),
        };
        let mut options = file_options();
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
            options.share_mode(FILE_SHARE_READ.0);
        }
        options.write(false);
        let file = parent.directory.open_with(&key, &options)?.into_std();
        // ponytail: keep legacy reads on the existing evidence and bounded-read implementation.
        let owned = OwnedFile::inspected_legacy(parent, binding, key, file)?;
        anyhow::ensure!(Instant::now() < deadline, "legacy inspection timed out");
        Ok(owned)
    }
    /// Retires an already adopted legacy file using exact journal-owned evidence.
    /// The caller must hold its object worker lease and authorize retirement.
    ///
    /// # Errors
    /// Preserves ambiguous files, changed bindings, and mismatched ownership evidence.
    pub fn retire_legacy(
        &self,
        relative_key: &str,
        identity: &str,
        bytes: u64,
        digest: [u8; 32],
        operation: &str,
    ) -> anyhow::Result<()> {
        let (parent, key) = self.legacy_parent(relative_key)?;
        parent.retire_with_binding(&key, identity, bytes, digest, operation, Some(self))
    }

    /// Acknowledges legacy retirement only after its owning catalog transition commits.
    ///
    /// # Errors
    /// Rejects changed bindings, receipt evidence, or unresolved retirement.
    pub fn acknowledge_legacy(
        &self,
        relative_key: &str,
        identity: &str,
        bytes: u64,
        digest: [u8; 32],
        operation: &str,
    ) -> anyhow::Result<()> {
        let (parent, key) = self.legacy_parent(relative_key)?;
        parent.acknowledge_with_binding(&key, identity, bytes, digest, operation, Some(self))
    }

    fn legacy_parent(&self, relative_key: &str) -> anyhow::Result<(Self, String)> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let components = components(relative_key)?;
        self.revalidate()?;
        validate_owner(&self.directory, 0o022)?;
        let mut directory = self.directory.try_clone()?;
        let mut path = self.path.clone();
        for component in &components[..components.len() - 1] {
            anyhow::ensure!(Instant::now() < deadline, "legacy inspection timed out");
            directory = directory.open_dir_nofollow(component)?;
            #[cfg(windows)]
            super::reject_reparse(&directory)?;
            validate_owner(&directory, 0o022)?;
            path.push(component);
        }
        let identity = identity(&directory)?;
        let parent = Self {
            path,
            directory,
            identity,
        };
        self.revalidate()?;
        parent.revalidate()?;
        anyhow::ensure!(Instant::now() < deadline, "legacy inspection timed out");
        Ok((
            parent,
            components
                .last()
                .expect("validated nonempty path")
                .to_string(),
        ))
    }
}

pub(super) fn components(key: &str) -> anyhow::Result<Vec<&str>> {
    anyhow::ensure!(
        !key.is_empty() && key.len() <= 4096 && !key.contains(['\\', ':', '\0']),
        "invalid legacy relative path"
    );
    let components: Vec<_> = key.split('/').take(17).collect();
    anyhow::ensure!(
        components.len() <= 16,
        "legacy path has too many components"
    );
    for component in &components {
        anyhow::ensure!(
            !component.is_empty()
                && *component != "."
                && *component != ".."
                && !component.ends_with(['.', ' '])
                && !component.chars().any(char::is_control),
            "invalid legacy path component"
        );
    }
    Ok(components)
}
