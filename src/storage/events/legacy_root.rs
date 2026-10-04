use crate::storage::{
    RecordingCatalogHandle,
    catalog::locations::{Reply, Request},
    volumes::validation::comparison_root,
};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub(super) fn contains(root: &Path, candidate: &Path, _was_offline: bool) -> bool {
    #[cfg(windows)]
    if _was_offline {
        // Match the captured-root comparison without resolving a replacement junction.
        return PathBuf::from(candidate.as_os_str().to_ascii_lowercase())
            .starts_with(PathBuf::from(root.as_os_str().to_ascii_lowercase()));
    }
    candidate.starts_with(root)
}

pub(super) fn prepare(
    catalog: &RecordingCatalogHandle,
    requested: &Path,
) -> anyhow::Result<(PathBuf, bool)> {
    let Reply::LegacyPaths(paths) = catalog.volume_location(Request::LegacyPaths)? else {
        anyhow::bail!("invalid legacy root reply");
    };
    let Some(paths) = paths else {
        fs::create_dir_all(requested)?;
        return Ok((requested.canonicalize()?, true));
    };
    let root = std::path::absolute(requested)?;
    anyhow::ensure!(
        comparison_root(&root)? == comparison_root(&paths.thumbnail_root)?,
        "captured thumbnail root changed"
    );
    match root.canonicalize() {
        Ok(root) => Ok((root, true)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok((normalize_missing(&root)?, false)),
        Err(error) => Err(error.into()),
    }
}

fn normalize_missing(root: &Path) -> anyhow::Result<PathBuf> {
    // ponytail: Resolve existing aliases once; keep the missing suffix without creating it.
    for ancestor in root.ancestors().skip(1).take(256) {
        match ancestor.canonicalize() {
            Ok(canonical) => return Ok(canonical.join(root.strip_prefix(ancestor)?)),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("thumbnail root has no available ancestor")
}
