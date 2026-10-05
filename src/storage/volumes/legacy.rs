//! Verifies catalog-known legacy files without taking over their cleanup or placement.

use crate::storage::catalog::{
    RecordingCatalogHandle,
    locations::{
        Reply, Request,
        legacy::inventory::{Action, Evidence, Reference},
    },
};

pub(crate) fn export_root_offline(
    catalog: Option<&RecordingCatalogHandle>,
    requested: &std::path::Path,
) -> anyhow::Result<bool> {
    let Some(catalog) = catalog else {
        return Ok(false);
    };
    let Reply::LegacyPaths(paths) = catalog.volume_location(Request::LegacyPaths)? else {
        anyhow::bail!("invalid legacy root reply");
    };
    let Some(paths) = paths else { return Ok(false) };
    let root = std::path::absolute(requested)?;
    anyhow::ensure!(
        super::validation::comparison_root(&root)?
            == super::validation::comparison_root(&paths.export_root)?,
        "captured export root changed"
    );
    match std::fs::metadata(root) {
        Ok(metadata) => {
            anyhow::ensure!(metadata.is_dir(), "captured export root is not a directory");
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn ensure_export_history_available(
    catalog: Option<&RecordingCatalogHandle>,
    root: &std::path::Path,
    history: &std::path::Path,
) -> anyhow::Result<()> {
    if export_root_offline(catalog, root)? {
        let root = super::validation::comparison_root(&std::path::absolute(root)?)?;
        let history = super::validation::comparison_root(&std::path::absolute(history)?)?;
        anyhow::ensure!(
            !history.starts_with(root),
            "captured export history is unavailable"
        );
    }
    Ok(())
}

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
    let (paths, role) = recording_role(catalog, reference)?;
    let path = role.path(&paths);
    let key = reference
        .path
        .strip_prefix(path)?
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("legacy recording path is not UTF-8"))?
        .replace('\\', "/");
    let root = match captured_root(catalog, role)? {
        Some(root) => root,
        None => super::root::Root::open(path)?,
    };
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

pub(crate) fn recording_role(
    catalog: &RecordingCatalogHandle,
    reference: &Reference,
) -> anyhow::Result<(
    crate::storage::catalog::locations::legacy::LegacyPaths,
    crate::storage::catalog::locations::legacy::roots::Role,
)> {
    use crate::storage::catalog::locations::legacy::roots::Role;
    let Reply::LegacyPaths(Some(paths)) = catalog.volume_location(Request::LegacyPaths)? else {
        anyhow::bail!("legacy roots have not been captured");
    };
    // ponytail: The two captured recording roots suffice; no filesystem discovery is needed.
    let role = [Role::Archive, Role::Active]
        .into_iter()
        .filter(|role| reference.path.starts_with(role.path(&paths)))
        .max_by_key(|role| role.path(&paths).components().count())
        .ok_or_else(|| anyhow::anyhow!("legacy recording is outside captured roots"))?;
    Ok((*paths, role))
}

pub(crate) fn verify_export(
    catalog: &RecordingCatalogHandle,
    reference: &Reference,
) -> anyhow::Result<Reference> {
    use crate::storage::catalog::locations::{Kind, legacy::roots::Role};
    anyhow::ensure!(
        reference.object.kind == Kind::Export,
        "invalid legacy export kind"
    );
    let root = captured_root(catalog, Role::Export)?
        .ok_or_else(|| anyhow::anyhow!("legacy export root identity has not been captured"))?;
    let key = reference
        .path
        .strip_prefix(root.path())?
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("legacy export path is not UTF-8"))?
        .replace('\\', "/");
    let mut file = root.inspect_legacy(&key)?;
    let (bytes, file_identity, digest) = file.inspect_evidence()?;
    let evidence = Evidence {
        bytes,
        file_identity,
        digest,
        catalog_identity: file.catalog_identity()?,
    };
    file.revalidate()?;
    if let Some(expected) = &reference.evidence {
        anyhow::ensure!(*expected == evidence, "legacy export evidence changed");
    }
    Ok(Reference {
        evidence: Some(evidence),
        ..reference.clone()
    })
}

#[cfg(test)]
mod tests;

/// Captures only the four configured media roots; unavailable roots are not created.
/// Later recovery must match these identities or require another explicit capture.
pub fn capture_roots(
    catalog: &RecordingCatalogHandle,
    paths: &crate::storage::catalog::locations::legacy::LegacyPaths,
) -> anyhow::Result<()> {
    use crate::storage::catalog::locations::{
        Binding,
        legacy::roots::{Capture, Role},
    };
    let mut capture = Capture {
        paths: paths.clone(),
        roots: Vec::with_capacity(4),
    };
    let mut pinned = Vec::with_capacity(4);
    let mut known = known_roots(catalog)?;
    for role in Role::ALL {
        let Ok(root) = super::root::Root::open(role.path(paths)) else {
            capture.roots.push((role, None));
            continue;
        };
        // ponytail: four known roots allow a direct scan; no filesystem discovery or cache.
        let shared = known.iter().find(|binding| {
            binding.filesystem == root.identity().filesystem
                && binding.root_identity == root.identity().directory
        });
        let binding = shared.cloned().unwrap_or_else(|| Binding {
            id: role.id().into(),
            generation: 1,
            root: role.path(paths).into(),
            filesystem: root.identity().filesystem.clone(),
            root_identity: root.identity().directory.clone(),
            writable: false,
            draining: false,
            limit_bytes: None,
            minimum_free_bytes: 0,
        });
        known.push(binding.clone());
        capture.roots.push((role, Some(binding)));
        pinned.push(root);
    }
    for root in &pinned {
        root.revalidate()?;
    }
    let reply = catalog.volume_location(Request::CaptureLegacyRoots(Box::new(capture)))?;
    anyhow::ensure!(reply == Reply::Bound, "invalid legacy capture reply");
    for root in &pinned {
        root.revalidate()?;
    }
    Ok(())
}

pub(crate) fn captured_root(
    catalog: &RecordingCatalogHandle,
    role: crate::storage::catalog::locations::legacy::roots::Role,
) -> anyhow::Result<Option<super::root::Root>> {
    use crate::storage::catalog::locations::legacy::roots::State;
    let Reply::LegacyRoot(state) = catalog.volume_location(Request::LegacyRoot(role))? else {
        anyhow::bail!("invalid legacy root reply");
    };
    let binding = match state {
        State::Uncaptured => return Ok(None),
        State::Offline => {
            anyhow::bail!("legacy root was unavailable at capture; explicit recapture is required")
        }
        State::Bound(binding) => binding,
    };
    Ok(Some(open_binding(&binding)?))
}

fn open_binding(
    binding: &crate::storage::catalog::locations::Binding,
) -> anyhow::Result<super::root::Root> {
    let root = super::root::Root::open(&binding.root)?;
    anyhow::ensure!(
        root.identity().filesystem == binding.filesystem
            && root.identity().directory == binding.root_identity,
        "captured legacy root identity changed"
    );
    Ok(root)
}

pub(crate) fn volume_root(
    catalog: &RecordingCatalogHandle,
    volume: &str,
) -> anyhow::Result<Option<super::root::Root>> {
    if !volume.starts_with("legacy-") {
        return Ok(None);
    }
    let binding = known_roots(catalog)?
        .into_iter()
        .find(|binding| binding.id == volume)
        .ok_or_else(|| anyhow::anyhow!("legacy volume identity has not been captured"))?;
    Ok(Some(open_binding(&binding)?))
}

fn known_roots(
    catalog: &RecordingCatalogHandle,
) -> anyhow::Result<Vec<crate::storage::catalog::locations::Binding>> {
    use crate::storage::catalog::locations::legacy::roots::{Role, State};
    let mut known = Vec::with_capacity(4);
    for role in Role::ALL {
        match catalog.volume_location(Request::LegacyRoot(role))? {
            Reply::LegacyRoot(State::Bound(binding)) => known.push(*binding),
            Reply::LegacyRoot(State::Uncaptured | State::Offline) => {}
            _ => anyhow::bail!("invalid legacy root reply"),
        }
    }
    Ok(known)
}
