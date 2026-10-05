use super::*;
use std::io::Write;

const HISTORY_BYTES_MAX: u64 = 8 * 1024 * 1024;

impl Root {
    pub(crate) fn initialize_history(&self, key: &str) -> anyhow::Result<()> {
        let id = history_key(key)?;
        self.sync()?;
        match self.directory.symlink_metadata(key) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // ponytail: one failed initialization stage blocks retry until operator review.
                let mut stage = self.create_file(&format!("{id}.tmp"))?;
                stage.file.write_all(b"{\"version\":1,\"jobs\":[]}\n")?;
                stage.publish_name(key)?;
            }
            Err(error) => return Err(error.into()),
        }
        crate::server::validate_export_history_snapshot(&self.read_history(key)?)
    }

    pub(crate) fn read_history(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        let mut file = self.history_file(key)?;
        let identity = file_identity(&file)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(HISTORY_BYTES_MAX + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            u64::try_from(bytes.len())? <= HISTORY_BYTES_MAX,
            "export history is too large"
        );
        self.check_history(key, &identity)?;
        Ok(bytes)
    }

    pub(crate) fn replace_history(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let id = history_key(key)?;
        anyhow::ensure!(
            u64::try_from(bytes.len())? <= HISTORY_BYTES_MAX,
            "export history is too large"
        );
        let original = self.history_file(key)?;
        let identity = file_identity(&original)?;
        // ponytail: One fixed staging name bounds failed writes; never overwrite an unknown stage.
        let mut temporary = self.create_file(&format!("{id}.tmp"))?;
        temporary.file.write_all(bytes)?;
        temporary.replace_history(key, &identity)
    }

    fn history_file(&self, key: &str) -> anyhow::Result<File> {
        history_key(key)?;
        self.revalidate()?;
        let file = self.directory.open_with(key, &file_options())?.into_std();
        file_identity(&file)?;
        anyhow::ensure!(
            file.metadata()?.len() <= HISTORY_BYTES_MAX,
            "export history is too large"
        );
        self.revalidate()?;
        Ok(file)
    }

    fn check_history(&self, key: &str, expected: &str) -> anyhow::Result<()> {
        let current = self.history_file(key)?;
        anyhow::ensure!(
            file_identity(&current)? == expected,
            "export history changed during access"
        );
        Ok(())
    }
}

impl OwnedFile {
    fn replace_history(mut self, target: &str, original: &str) -> anyhow::Result<()> {
        self.revalidate()?;
        self.file.sync_all()?;
        self.root.sync()?;
        self.root.check_history(target, original)?;
        #[cfg(windows)]
        crate::storage::long_term::inspection::removal::windows::replace_to(
            &self.file,
            &self.root.directory,
            std::ffi::OsStr::new(target),
        )?;
        #[cfg(unix)]
        rustix::fs::renameat(
            &self.root.directory,
            &self.key,
            &self.root.directory,
            target,
        )?;
        #[cfg(not(any(unix, windows)))]
        anyhow::bail!("metadata replacement is unsupported on this platform");
        self.key = target.to_owned();
        self.revalidate()?;
        self.root.sync()?;
        self.revalidate()
    }
}

fn history_key(key: &str) -> anyhow::Result<uuid::Uuid> {
    let id = key
        .strip_prefix("exports-")
        .and_then(|key| key.strip_suffix(".json"))
        .ok_or_else(|| anyhow::anyhow!("invalid managed history filename"))?;
    let id = uuid::Uuid::parse_str(id)?;
    anyhow::ensure!(
        format!("exports-{id}.json") == key,
        "invalid managed history filename"
    );
    Ok(id)
}

#[cfg(test)]
mod tests;
