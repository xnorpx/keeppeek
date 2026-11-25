//! Confirms that a pending restore can still recover a newly claimed archive deletion.

use super::*;
use crate::storage::archive::settings::Settings;

/// Called under the same configuration update lock as restore staging and archive claims.
pub fn check_staged_archive_deletion(config_path: &Path, active: &Settings) -> anyhow::Result<()> {
    let path = restore_journal_path(config_path);
    if !path.try_exists()? {
        return Ok(());
    }
    let journal = load_journal(&path)?;
    match journal.state {
        RestoreJournalState::RolledBack | RestoreJournalState::Complete => return Ok(()),
        RestoreJournalState::Ready => {}
        _ => anyhow::bail!("restore recovery is in progress"),
    }
    anyhow::ensure!(
        !journal.targets.iter().any(|target| target.database),
        "pending database restore can replace archive deletion records"
    );
    let config = canonical_target_path(config_path)?;
    let secrets = canonical_target_path(&config::secrets_path(config_path))?;
    let staged = |path: &Path| journal.targets.iter().find(|item| item.target == path);
    let next_config = staged(&config);
    let next_secrets = staged(&secrets);
    if next_config.is_none() && next_secrets.is_none() {
        return Ok(());
    }
    let config = configuration_bytes(&config, next_config, false)?;
    let secrets = configuration_bytes(&secrets, next_secrets, true)?;
    let candidate = crate::backup::validated_native_configuration(&config, &secrets)?;
    anyhow::ensure!(
        candidate.storage.archive.is_some_and(|next| next.enabled
            && next.target_id == active.target_id
            && next.root == active.root),
        "pending restore changes archive deletion target"
    );
    Ok(())
}

fn configuration_bytes(
    path: &Path,
    staged: Option<&RestoreJournalTarget>,
    optional: bool,
) -> anyhow::Result<Vec<u8>> {
    let path = staged.map_or(path, |item| item.staged.as_path());
    let file = match std::fs::File::open(path) {
        Err(error)
            if optional && staged.is_none() && error.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(Vec::new());
        }
        other => other?,
    };
    let limit = BackupSection::RuntimeConfig.maximum_document_bytes();
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= limit,
        "restore configuration exceeds the document limit"
    );
    if let Some(staged) = staged {
        anyhow::ensure!(
            staged.prepared
                && crate::backup::encode_lower_hex(Sha256::digest(&bytes))
                    == staged.expected_sha256,
            "staged restore configuration has changed"
        );
    }
    Ok(bytes)
}
