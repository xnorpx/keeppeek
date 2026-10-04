use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

fn fixture() -> anyhow::Result<(std::path::PathBuf, Root)> {
    let (path, root) = super::super::file::tests::fixture()?;
    std::fs::create_dir_all(path.join("front/main/2026-10-04/12"))?;
    std::fs::write(path.join(KEY), b"legacy recording")?;
    Ok((path, root))
}

const KEY: &str = "front/main/2026-10-04/12/12-34-56 recording.mp4";

#[test]
fn nested_retirement_replays_then_acknowledges_without_removing_directories() -> anyhow::Result<()>
{
    let (path, root) = fixture()?;
    let (bytes, identity, digest) = root.inspect_legacy(KEY)?.inspect_evidence()?;
    let operation = uuid::Uuid::new_v4().to_string();
    root.retire_legacy(KEY, &identity, bytes, digest, &operation)?;
    assert!(!path.join(KEY).exists());
    let parent = path.join(KEY).parent().unwrap().to_owned();
    assert!(parent.join(".retired").join(&operation).is_dir());
    root.retire_legacy(KEY, &identity, bytes, digest, &operation)?;
    assert!(
        root.acknowledge_legacy(KEY, "unrelated", bytes, digest, &operation)
            .is_err()
    );
    root.acknowledge_legacy(KEY, &identity, bytes, digest, &operation)?;
    root.acknowledge_legacy(KEY, &identity, bytes, digest, &operation)?;
    assert!(parent.is_dir());
    assert!(!parent.join(".retired").join(operation).exists());
    Ok(())
}

#[test]
fn nested_retirement_rejects_changed_evidence_and_unexplained_absence() -> anyhow::Result<()> {
    let (path, root) = fixture()?;
    let (bytes, identity, digest) = root.inspect_legacy(KEY)?.inspect_evidence()?;
    for (expected_identity, expected_bytes, expected_digest) in [
        ("unrelated", bytes, digest),
        (&identity, bytes + 1, digest),
        (&identity, bytes, [0; 32]),
    ] {
        assert!(
            root.retire_legacy(
                KEY,
                expected_identity,
                expected_bytes,
                expected_digest,
                &uuid::Uuid::new_v4().to_string()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path.join(KEY))?, b"legacy recording");
    }
    assert!(
        root.retire_owned(
            KEY,
            &identity,
            bytes,
            digest,
            &uuid::Uuid::new_v4().to_string()
        )
        .is_err()
    );
    std::fs::remove_file(path.join(KEY))?;
    assert!(
        root.retire_legacy(
            KEY,
            &identity,
            bytes,
            digest,
            &uuid::Uuid::new_v4().to_string()
        )
        .is_err()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn legacy_receipt_rejects_new_binding_even_when_parent_identity_survives() -> anyhow::Result<()> {
    let (path, root) = fixture()?;
    let (bytes, identity, digest) = root.inspect_legacy(KEY)?.inspect_evidence()?;
    let operation = uuid::Uuid::new_v4().to_string();
    root.retire_legacy(KEY, &identity, bytes, digest, &operation)?;
    let moved = path.with_extension("old-binding");
    std::fs::rename(&path, &moved)?;
    std::fs::create_dir(&path)?;
    std::fs::rename(moved.join("front"), path.join("front"))?;
    let replacement = Root::open(&path)?;
    assert!(
        root.retire_legacy(KEY, &identity, bytes, digest, &operation)
            .is_err()
    );
    assert!(
        replacement
            .retire_legacy(KEY, &identity, bytes, digest, &operation)
            .is_err()
    );
    assert!(
        replacement
            .acknowledge_legacy(KEY, &identity, bytes, digest, &operation)
            .is_err()
    );
    assert!(
        path.join(KEY)
            .parent()
            .unwrap()
            .join(".retired")
            .join(operation)
            .is_dir()
    );
    Ok(())
}

#[test]
fn nested_legacy_reader_preserves_bytes_identity_and_write_restrictions() -> anyhow::Result<()> {
    let (path, root) = fixture()?;
    let mut file = root.inspect_legacy(KEY)?;
    let (bytes, identity, digest) = file.inspect_evidence()?;
    assert_eq!(bytes, 16);
    assert_eq!(
        digest,
        <[u8; 32]>::from(Sha256::digest(b"legacy recording"))
    );
    #[cfg(windows)]
    assert_eq!(identity.split_once(':').unwrap().1.len(), 32);
    assert_eq!(root.inspect_legacy(KEY)?.inspect_evidence()?.1, identity);
    let mut read = Vec::new();
    file.read_to_end(&mut read)?;
    assert_eq!(read, b"legacy recording");
    assert!(file.file_mut().write_all(b"changed").is_err());
    assert!(file.evidence().is_err());
    assert!(
        file.publish_staged(&format!("{}.mp4", uuid::Uuid::new_v4()))
            .is_err()
    );
    assert!(root.create_file(KEY).is_err());
    assert_eq!(std::fs::read(path.join(KEY))?, b"legacy recording");
    Ok(())
}

#[test]
fn legacy_paths_are_bounded_and_never_create_or_adopt_links() -> anyhow::Result<()> {
    let (path, root) = fixture()?;
    for key in [
        "",
        "/absolute.mp4",
        "../outside.mp4",
        "front/../x",
        "front/./x",
        "front//x",
        "C:/x",
        "front\\x",
        "front/x:stream",
        "front/trailing.",
        "front/trailing ",
    ] {
        assert!(root.inspect_legacy(key).is_err(), "accepted {key}");
    }
    assert!(root.inspect_legacy(&"x/".repeat(17)).is_err());
    assert!(root.inspect_legacy(&"x".repeat(4097)).is_err());
    assert!(root.inspect_legacy("missing.mp4").is_err());
    assert!(!path.join("missing.mp4").exists());
    std::fs::hard_link(path.join(KEY), path.join("linked.mp4"))?;
    assert!(root.inspect_legacy(KEY).is_err());
    assert!(root.inspect_legacy("linked.mp4").is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn legacy_reader_rejects_symlinks_and_replaced_parent_or_binding() -> anyhow::Result<()> {
    let (path, root) = fixture()?;
    std::os::unix::fs::symlink(path.join("front"), path.join("alias"))?;
    assert!(
        root.inspect_legacy("alias/main/2026-10-04/12/12-34-56 recording.mp4")
            .is_err()
    );
    std::os::unix::fs::symlink(path.join(KEY), path.join("linked.mp4"))?;
    assert!(root.inspect_legacy("linked.mp4").is_err());
    let mut file = root.inspect_legacy(KEY)?;
    std::fs::rename(path.join("front"), path.join("original"))?;
    std::fs::create_dir_all(path.join("front/main/2026-10-04/12"))?;
    std::fs::write(path.join(KEY), b"legacy recording")?;
    assert!(file.read(&mut [0; 16]).is_err());
    let mut file = root.inspect_legacy(KEY)?;
    let moved = path.with_extension("old");
    std::fs::rename(&path, &moved)?;
    std::fs::create_dir(&path)?;
    std::fs::rename(moved.join("front"), path.join("front"))?;
    assert!(
        file.inspect_evidence().is_err(),
        "original binding was replaced"
    );
    Ok(())
}

#[cfg(windows)]
#[test]
fn legacy_reader_pins_name_against_replacement_and_truncation() -> anyhow::Result<()> {
    let (path, root) = fixture()?;
    let mut file = root.inspect_legacy(KEY)?;
    assert!(std::fs::rename(path.join(KEY), path.join("moved.mp4")).is_err());
    assert!(std::fs::write(path.join(KEY), b"changed").is_err());
    assert_eq!(file.inspect_evidence()?.0, 16);
    Ok(())
}
