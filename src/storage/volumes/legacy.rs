//! Verifies catalog-known legacy files without taking over their cleanup or placement.

use crate::storage::catalog::{
    RecordingCatalogHandle,
    locations::{
        Reply, Request,
        legacy::inventory::{Action, Evidence, Reference},
    },
};

/// Verifies one recording through its captured legacy roots and the current owner revision.
///
/// # Errors
/// Preserves unresolved references when roots, files, or owner evidence are unavailable or changed.
pub fn verify_recording(
    catalog: &RecordingCatalogHandle,
    reference: &Reference,
) -> anyhow::Result<Reference> {
    let Reply::LegacyReference(Some(current)) = catalog.volume_location(
        Request::LegacyInventory(Action::Lookup(reference.object.clone())),
    )?
    else {
        anyhow::bail!("legacy owner is unavailable");
    };
    anyhow::ensure!(
        current.path == reference.path && current.revision == reference.revision,
        "legacy reference changed"
    );
    let Reply::LegacyPaths(Some(paths)) = catalog.volume_location(Request::LegacyPaths)? else {
        anyhow::bail!("legacy roots have not been captured");
    };
    // ponytail: the two captured recording roots suffice; no filesystem discovery is needed.
    let path = [&paths.archive_root, &paths.active_root]
        .into_iter()
        .filter(|root| reference.path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| anyhow::anyhow!("legacy recording is outside captured roots"))?;
    let key = reference
        .path
        .strip_prefix(path)?
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("legacy recording path is not UTF-8"))?
        .replace('\\', "/");
    let root = super::root::Root::open(path)?;
    let mut file = root.inspect_legacy(&key)?;
    let (bytes, file_identity, digest) = file.inspect_evidence()?;
    let catalog_identity = file.catalog_identity()?;
    let reply = catalog.volume_location(Request::LegacyInventory(Action::Verify(
        Box::new(reference.clone()),
        Evidence {
            file_identity,
            catalog_identity,
            bytes,
            digest,
        },
    )))?;
    file.revalidate()?;
    let Reply::LegacyReference(Some(verified)) = reply else {
        anyhow::bail!("invalid legacy verification reply");
    };
    Ok(*verified)
}

#[cfg(test)]
mod tests;
