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
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // ponytail: retain the missing root without creating directories or detaching references.
            // Windows readers compare canonical child paths, which use the verbatim prefix.
            #[cfg(windows)]
            let root = PathBuf::from(format!("\\\\?\\{}", root.display()));
            Ok((root, false))
        }
        Err(error) => Err(error.into()),
    }
}
