use super::*;
use crate::storage::{
    catalog::RecordingCatalog,
    volumes::{PlacementRule, PlacementStrategy, VolumeId},
};

pub fn fixture(limit: u64) -> anyhow::Result<(PathBuf, RecordingCatalog, Manager)> {
    let base = std::env::temp_dir();
    #[cfg(unix)]
    let base = std::fs::canonicalize(base)?;
    let path = base.join(format!("keeppeek-volume-runtime-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path)?;
    let root = path.join("primary");
    create_root(&root)?;
    let catalog = RecordingCatalog::open(&path.join("catalog.db"))?;
    let configuration = VolumeConfiguration {
        volumes: vec![volume("primary", root, limit)],
        placement: vec![
            rule(VolumeRole::Active, &["primary"], false),
            rule(VolumeRole::Export, &["primary"], false),
        ],
    };
    let manager = Manager::new(configuration, catalog.handle())?;
    Ok((path, catalog, manager))
}

pub(super) fn create_root(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(path)?;
    #[cfg(windows)]
    anyhow::ensure!(
        std::process::Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/.github/scripts/protect-test-directory.ps1"
            ))
            .arg("-Directory")
            .arg(path)
            .status()?
            .success(),
        "cannot protect runtime fixture"
    );
    Ok(())
}

pub(super) fn volume(id: &str, root: PathBuf, limit: u64) -> Volume {
    Volume {
        id: VolumeId::parse(id).unwrap(),
        root,
        roles: vec![VolumeRole::Active, VolumeRole::Export],
        state: VolumeState::Enabled,
        priority: 0,
        capacity_bytes: Some(limit),
        minimum_free_bytes: 0,
        warning_free_bytes: 0,
        critical_free_bytes: 0,
        sources: vec![],
        groups: vec![],
    }
}

fn rule(role: VolumeRole, ids: &[&str], allow_fallback: bool) -> PlacementRule {
    PlacementRule {
        role,
        source: None,
        group: None,
        candidates: ids.iter().map(|id| VolumeId::parse(id).unwrap()).collect(),
        strategy: PlacementStrategy::Priority,
        allow_fallback,
    }
}

fn object() -> Object {
    Object {
        kind: Kind::Export,
        id: uuid::Uuid::new_v4().to_string(),
    }
}

#[test]
fn offline_root_recovers_without_restarting_the_manager() -> anyhow::Result<()> {
    let (path, catalog, original) = fixture(GROWTH_BYTES)?;
    let mut configuration = original.inner.configuration.clone();
    configuration.volumes[0].root = path.join("late-volume");
    configuration.volumes[0].id = VolumeId::parse("late")?;
    configuration.placement = vec![rule(VolumeRole::Export, &["late"], false)];
    let manager = Manager::new(configuration, catalog.handle())?;
    assert!(
        manager
            .reserve(VolumeRole::Export, "camera", &[], object(), 8)
            .is_err()
    );
    create_root(&path.join("late-volume"))?;
    manager.recover_roots()?;
    let reservation = manager.reserve(VolumeRole::Export, "camera", &[], object(), 8)?;
    let mut writer = reservation.expect("recovered root accepts writes").open()?;
    writer.write_all(&[1; 8])?;
    assert_eq!(writer.evidence()?.bytes, 8);
    catalog.shutdown();
    Ok(())
}

#[test]
fn offline_root_recovery_refuses_a_replacement_directory() -> anyhow::Result<()> {
    let (path, catalog, original) = fixture(GROWTH_BYTES)?;
    let configuration = original.inner.configuration.clone();
    drop(original);
    std::fs::rename(path.join("primary"), path.join("original"))?;
    let manager = Manager::new(configuration, catalog.handle())?;
    create_root(&path.join("primary"))?;
    manager.recover_roots()?;
    assert!(
        manager
            .reserve(VolumeRole::Export, "camera", &[], object(), 8)
            .is_err()
    );
    assert!(path.join("original").is_dir());
    std::fs::remove_dir(path.join("primary"))?;
    std::fs::rename(path.join("original"), path.join("primary"))?;
    manager.recover_roots()?;
    assert!(
        manager
            .reserve(VolumeRole::Export, "camera", &[], object(), 8)?
            .is_some()
    );
    catalog.shutdown();
    Ok(())
}

#[test]
fn root_recovery_does_not_activate_disabled_volumes() -> anyhow::Result<()> {
    let (_, catalog, original) = fixture(GROWTH_BYTES)?;
    let mut configuration = original.inner.configuration.clone();
    configuration.volumes[0].state = VolumeState::Disabled;
    let manager = Manager::new(configuration, catalog.handle())?;
    manager.recover_roots()?;
    assert!(manager.inner.root(0).is_err());
    assert!(
        manager
            .reserve(VolumeRole::Export, "camera", &[], object(), 8)
            .is_err()
    );
    catalog.shutdown();
    Ok(())
}

#[test]
fn writer_checkpoints_growth_and_rejects_sparse_seeks_and_unknown_ownership() -> anyhow::Result<()>
{
    let (_, catalog, manager) = fixture(2 * GROWTH_BYTES)?;
    let reservation = manager
        .reserve(VolumeRole::Export, "camera", &[], object(), 8)?
        .unwrap();
    let mut file = reservation.open()?;
    file.write_all(&[1; 8])?;
    assert!(file.seek(SeekFrom::Start(9)).is_err());
    assert_eq!(file.stream_position()?, 8);
    file.write_all(&[2])?;
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        panic!("missing usage");
    };
    assert_eq!(usage[0].allocated_bytes, GROWTH_BYTES);
    assert_eq!(usage[0].reserved_bytes, GROWTH_BYTES - 8);
    file.write_all(&vec![3; usize::try_from(GROWTH_BYTES - 9)?])?;
    catalog.shutdown();
    assert!(file.write_all(&[4]).is_err());
    file.rewind()?;
    assert!(file.write_all(&[5]).is_err());
    assert!(file.evidence().is_err());
    Ok(())
}

#[test]
fn reserved_writer_grows_and_refuses_overcap_before_writing() -> anyhow::Result<()> {
    let (_, catalog, manager) = fixture(32)?;
    let reservation = manager
        .reserve(VolumeRole::Export, "camera", &[], object(), 8)?
        .unwrap();
    let path = reservation.path().to_path_buf();
    let mut file = reservation.open()?;
    file.write_all(&[1; 16])?;
    file.write_all(&[2; 16])?;
    assert!(file.write_all(&[3]).is_err());
    assert_eq!(std::fs::metadata(path)?.len(), 32);
    assert!(file.evidence().is_err());
    catalog.shutdown();
    Ok(())
}

#[test]
fn export_publication_requires_evidence_and_seals_writes() -> anyhow::Result<()> {
    let (_, catalog, manager) = fixture(128)?;
    let object = object();
    let reservation = manager
        .reserve(VolumeRole::Export, "camera", &[], object.clone(), 8)?
        .unwrap();
    let mut file = reservation.open()?;
    file.write_all(b"export")?;
    let publication = file.evidence()?;
    let mut forged = publication.clone();
    forged.digest = [0; 32];
    assert!(file.publish(forged).is_err());
    file.publish(publication.clone())?;
    file.publish(publication.clone())?;
    assert!(file.write_all(b"change").is_err());
    let Reply::Location(Some(location)) =
        catalog.handle().volume_location(Request::Lookup(object))?
    else {
        panic!("missing published export");
    };
    assert_eq!(location.bytes, 6);
    assert_eq!(location.digest, publication.digest);
    catalog.shutdown();
    Ok(())
}

#[test]
fn unmatched_rule_is_none_but_offline_policy_never_uses_legacy() -> anyhow::Result<()> {
    let (path, catalog, manager) = fixture(128)?;
    assert!(
        manager
            .reserve(
                VolumeRole::Thumbnail,
                "camera",
                &[],
                Object {
                    kind: Kind::Thumbnail,
                    id: uuid::Uuid::new_v4().to_string()
                },
                8
            )?
            .is_none()
    );
    let mut configuration = manager.inner.configuration.clone();
    configuration
        .volumes
        .push(volume("missing", path.join("missing"), 128));
    configuration.placement = vec![rule(VolumeRole::Export, &["missing", "primary"], false)];
    let unavailable = Manager::new(configuration.clone(), catalog.handle())?;
    assert!(
        unavailable
            .reserve(VolumeRole::Export, "camera", &[], object(), 8)
            .is_err()
    );
    configuration.placement[0].allow_fallback = true;
    let fallback = Manager::new(configuration, catalog.handle())?;
    let reservation = fallback
        .reserve(VolumeRole::Export, "camera", &[], object(), 8)?
        .unwrap();
    assert!(reservation.path().starts_with(path.join("primary")));
    catalog.shutdown();
    Ok(())
}

#[test]
fn file_never_switches_volume_and_dropped_failures_retain_ownership() -> anyhow::Result<()> {
    let (path, catalog, manager) = fixture(8)?;
    let secondary = path.join("secondary");
    create_root(&secondary)?;
    let mut configuration = manager.inner.configuration.clone();
    configuration
        .volumes
        .push(volume("secondary", secondary.clone(), 128));
    configuration.placement = vec![rule(VolumeRole::Export, &["primary", "secondary"], true)];
    let manager = Manager::new(configuration, catalog.handle())?;
    let reservation = manager
        .reserve(VolumeRole::Export, "camera", &[], object(), 8)?
        .unwrap();
    assert!(reservation.path().starts_with(path.join("primary")));
    let mut file = reservation.open()?;
    assert!(file.write_all(&[1; 9]).is_err());
    assert!(file.write_all(&[2; 8]).is_err());
    assert!(file.evidence().is_err());
    assert_eq!(file.file.file_mut().metadata()?.len(), 0);
    drop(file);
    assert_eq!(std::fs::read_dir(secondary)?.count(), 0);
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        panic!("missing usage");
    };
    assert_eq!(
        usage
            .iter()
            .find(|item| item.volume == "primary")
            .unwrap()
            .reserved_bytes,
        8
    );
    assert_eq!(
        usage
            .iter()
            .find(|item| item.volume == "secondary")
            .unwrap()
            .reserved_bytes,
        0
    );
    catalog.shutdown();
    Ok(())
}

#[test]
fn rounded_growth_retries_exact_need_once_and_drop_keeps_reservation() -> anyhow::Result<()> {
    let (_, catalog, manager) = fixture(2 * GROWTH_BYTES)?;
    let other = manager
        .reserve(
            VolumeRole::Export,
            "camera",
            &[],
            object(),
            2 * GROWTH_BYTES - 20,
        )?
        .unwrap();
    let reservation = manager
        .reserve(VolumeRole::Export, "camera", &[], object(), 10)?
        .unwrap();
    let mut file = reservation.open()?;
    file.write_all(&[7; 15])?;
    assert_eq!(file.evidence()?.bytes, 15);
    drop(file);
    drop(other);
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        panic!("missing usage");
    };
    assert_eq!(usage[0].reserved_bytes, 2 * GROWTH_BYTES - 5);
    catalog.shutdown();
    Ok(())
}

#[test]
fn conflicting_leaf_does_not_overwrite_or_release_committed_reservation() -> anyhow::Result<()> {
    let (_, catalog, manager) = fixture(128)?;
    let reservation = manager
        .reserve(VolumeRole::Export, "camera", &[], object(), 8)?
        .unwrap();
    let path = reservation.path().to_path_buf();
    std::fs::write(&path, b"existing")?;
    assert!(reservation.open().is_err());
    assert_eq!(std::fs::read(path)?, b"existing");
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        panic!("missing usage");
    };
    assert_eq!(usage[0].reserved_bytes, 8);
    catalog.shutdown();
    Ok(())
}
