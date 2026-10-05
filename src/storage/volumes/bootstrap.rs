//! Initializes a fresh installation before any media worker can start.

use super::{
    PlacementRule, PlacementStrategy, Volume, VolumeConfiguration, VolumeId, VolumeRole,
    VolumeState, root::Root,
};
use crate::config::{MetadataBinding, StorageToml};
use crate::storage::catalog::authority::Lease;
use std::path::Path;

// A fixed name lets an interrupted first startup reopen its catalog and history.
const BOOTSTRAP_ID: &str = "00000000-0000-4000-8000-000000000001";

/// Creates default owners and binds metadata before media workers start.
///
/// # Errors
/// Rejects invalid placement, unavailable metadata, unsafe roots, and incomplete history.
pub fn initialize(storage: &mut StorageToml, base: &Path) -> anyhow::Result<()> {
    if storage.metadata.is_some() {
        return Ok(());
    }
    anyhow::ensure!(
        storage.recording_catalog_path.is_none(),
        "configure a metadata volume instead of a catalog path"
    );
    if storage.named_volumes.is_none() {
        let configuration = defaults(storage, base)?;
        configuration.validate()?;
        let root = Root::open(base)?.create_private_child("storage")?;
        for volume in &configuration.volumes {
            root.create_private_child(volume.id.as_str())?;
        }
        storage.named_volumes = Some(configuration);
    }
    let configuration = storage.named_volumes.as_ref().expect("initialized volumes");
    configuration.validate()?;
    let rule = configuration
        .placement
        .iter()
        .find(|rule| rule.role == VolumeRole::Metadata)
        .ok_or_else(|| anyhow::anyhow!("a metadata placement rule is required"))?;
    let volume = configuration
        .volumes
        .iter()
        .find(|volume| volume.id == rule.candidates[0])
        .expect("validated metadata candidate");
    anyhow::ensure!(
        volume.state == VolumeState::Enabled,
        "metadata volume must be enabled"
    );
    let root = Root::open(&volume.root)?;
    let catalog_file = format!("catalog-{BOOTSTRAP_ID}.db");
    let history_file = format!("exports-{BOOTSTRAP_ID}.json");
    root.initialize_history(&history_file)?;
    let mut lease = Lease::acquire(&volume.root.join(&catalog_file))?;
    lease.require_root(&root)?;
    let connection = lease.connect()?;
    let authority = lease.initialize(&connection)?;
    crate::backup::database::checkpoint(&connection)?;
    root.sync()?;
    storage.metadata = Some(MetadataBinding {
        volume_id: volume.id.clone(),
        catalog_file,
        history_file,
        catalog_id: authority.catalog_id,
        generation: authority.generation,
        filesystem: root.identity().filesystem.clone(),
        root_identity: root.identity().directory.clone(),
    });
    Ok(())
}

fn defaults(storage: &StorageToml, base: &Path) -> anyhow::Result<VolumeConfiguration> {
    storage.validate_safety_thresholds()?;
    let safety = crate::storage::StorageConfig::from_toml(storage)
        .safety_policy()
        .evaluate(crate::storage::safety::FilesystemCapacity {
            total_bytes: 0,
            available_bytes: 0,
            keeppeek_bytes: 0,
        });
    let definitions = [
        (
            "media",
            vec![VolumeRole::Active, VolumeRole::Archive],
            storage.long_term_max_gb.saturating_mul(1 << 30),
        ),
        ("exports", vec![VolumeRole::Export], 0),
        (
            "images",
            vec![VolumeRole::Thumbnail],
            storage.event_thumbnail_max_mb.saturating_mul(1 << 20),
        ),
        ("metadata", vec![VolumeRole::Metadata], 0),
    ];
    let mut configuration = VolumeConfiguration::default();
    for (name, roles, cap) in definitions {
        let id = VolumeId::parse(name)?;
        for role in &roles {
            configuration.placement.push(PlacementRule {
                role: *role,
                source: None,
                group: None,
                candidates: vec![id.clone()],
                strategy: PlacementStrategy::Priority,
                allow_fallback: false,
            });
        }
        configuration.volumes.push(Volume {
            id,
            root: base.join("storage").join(name),
            roles,
            state: VolumeState::Enabled,
            priority: 0,
            capacity_bytes: (cap != 0).then_some(cap),
            minimum_free_bytes: storage.minimum_free_gb.saturating_mul(1 << 30),
            warning_free_bytes: safety.warning_free_bytes,
            critical_free_bytes: storage.critical_free_gb.saturating_mul(1 << 30),
            sources: vec![],
            groups: vec![],
        });
    }
    Ok(configuration)
}

#[cfg(test)]
mod tests {
    use super::initialize;
    use crate::{
        config::StorageToml,
        storage::volumes::{VolumeRole, VolumeState},
    };

    fn fixture() -> anyhow::Result<std::path::PathBuf> {
        let parent = std::env::temp_dir();
        #[cfg(unix)]
        let parent = parent.canonicalize()?;
        let base = parent.join(format!("keeppeek-bootstrap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&base)?;
        #[cfg(windows)]
        anyhow::ensure!(
            std::process::Command::new("powershell.exe")
                .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/.github/scripts/protect-test-directory.ps1"
                ))
                .arg("-Directory")
                .arg(&base)
                .status()?
                .success(),
            "cannot protect bootstrap fixture"
        );
        Ok(base)
    }

    #[test]
    fn fresh_bootstrap_persists_roles_caps_history_and_catalog_authority() -> anyhow::Result<()> {
        let base = fixture()?;
        let mut storage = StorageToml {
            long_term_max_gb: 3,
            event_thumbnail_max_mb: 7,
            ..StorageToml::default()
        };
        initialize(&mut storage, &base)?;
        let configuration = storage.named_volumes.as_ref().unwrap();
        configuration.validate()?;
        assert_eq!(configuration.volumes.len(), 4);
        assert_eq!(configuration.placement.len(), 5);
        for (id, roles, cap) in [
            (
                "media",
                vec![VolumeRole::Active, VolumeRole::Archive],
                Some(3 * 1024_u64.pow(3)),
            ),
            ("exports", vec![VolumeRole::Export], None),
            (
                "images",
                vec![VolumeRole::Thumbnail],
                Some(7 * 1024_u64.pow(2)),
            ),
            ("metadata", vec![VolumeRole::Metadata], None),
        ] {
            let volume = configuration
                .volumes
                .iter()
                .find(|v| v.id.as_str() == id)
                .unwrap();
            assert_eq!(volume.root, base.join("storage").join(id));
            assert_eq!(volume.roles, roles);
            assert_eq!(volume.state, VolumeState::Enabled);
            assert_eq!(volume.capacity_bytes, cap);
            crate::storage::volumes::root::Root::open(&volume.root)?;
            for role in roles {
                let rule = configuration
                    .placement
                    .iter()
                    .find(|r| r.role == role)
                    .unwrap();
                assert_eq!(rule.candidates.as_slice(), std::slice::from_ref(&volume.id));
                assert!(rule.source.is_none() && rule.group.is_none() && !rule.allow_fallback);
            }
        }
        assert_initial_metadata(&storage)?;
        std::fs::remove_dir_all(base)?;
        Ok(())
    }

    #[test]
    fn default_volumes_derive_an_omitted_warning_threshold() -> anyhow::Result<()> {
        let settings = StorageToml {
            minimum_free_gb: 8,
            warning_free_gb: 0,
            ..StorageToml::default()
        };
        let configuration = super::defaults(&settings, &std::path::absolute(".")?)?;
        configuration.validate()?;
        assert!(
            configuration
                .volumes
                .iter()
                .all(|volume| volume.warning_free_bytes
                    >= volume.minimum_free_bytes.max(volume.critical_free_bytes))
        );
        Ok(())
    }

    fn assert_initial_metadata(storage: &StorageToml) -> anyhow::Result<()> {
        let binding = storage.metadata.as_ref().unwrap();
        let (catalog, history) = binding.paths(storage)?;
        assert_eq!(
            binding.catalog_file,
            "catalog-00000000-0000-4000-8000-000000000001.db"
        );
        assert_eq!(
            binding.history_file,
            "exports-00000000-0000-4000-8000-000000000001.json"
        );
        let bytes = std::fs::read(history)?;
        crate::server::validate_export_history_snapshot(&bytes)?;
        let history: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(history["jobs"].as_array().unwrap().len(), 0);
        let mut lease = crate::storage::catalog::authority::Lease::acquire(&catalog)?;
        let database = lease.database()?;
        let connection = database.connect()?;
        assert_eq!(lease.verify(&connection)?, binding.authority());
        drop(connection);
        drop(database);
        drop(lease);
        Ok(())
    }

    #[test]
    fn replay_of_persisted_binding_does_not_recreate_missing_metadata_root() -> anyhow::Result<()> {
        let base = fixture()?;
        let mut storage = StorageToml::default();
        initialize(&mut storage, &base)?;
        let persisted = toml::to_string(&storage)?;
        let binding = storage.metadata.as_ref().unwrap().clone();
        let (catalog, _) = binding.paths(&storage)?;
        let root = catalog.parent().unwrap();
        let offline = base.join("offline-metadata");
        std::fs::rename(root, &offline)?;
        let mut restored: StorageToml = toml::from_str(&persisted)?;
        initialize(&mut restored, &base)?;
        assert_eq!(toml::to_string(&restored)?, persisted);
        assert!(!root.exists(), "bootstrap recreated a bound offline root");
        assert!(
            crate::storage::catalog::RecordingCatalog::open_managed(
                &catalog,
                &binding.authority(),
                &binding.root_identity()
            )
            .is_err()
        );
        assert!(!root.exists(), "managed catalog opening recreated the root");
        std::fs::rename(&offline, root)?;
        let catalog = crate::storage::catalog::RecordingCatalog::open_managed(
            &catalog,
            &binding.authority(),
            &binding.root_identity(),
        )?;
        catalog.shutdown();
        std::fs::remove_dir_all(base)?;
        Ok(())
    }
}
