use super::*;
use std::io::Write;

fn staged(root: &Root) -> anyhow::Result<(String, OwnedFile, String, [u8; 32])> {
    let key = format!("{}.tmp", uuid::Uuid::new_v4());
    let mut file = root.create_file(&key)?;
    file.file_mut().write_all(b"complete")?;
    let (_, identity, digest) = file.evidence()?;
    Ok((key, file, identity, digest))
}

#[test]
fn publication_renames_pinned_file_and_preserves_identity_and_digest() -> anyhow::Result<()> {
    let (path, root) = super::tests::fixture()?;
    let (key, file, identity, digest) = staged(&root)?;
    let target = format!("{}.mp4", uuid::Uuid::new_v4());
    file.publish_staged(&target)?;
    assert!(!path.join(key).exists());
    let mut published = root.open_owned(&target, &identity, 8)?;
    assert_eq!(published.inspect_evidence()?, (8, identity, digest));
    assert!(published.file_mut().write_all(b"overwrite").is_err());
    Ok(())
}

#[test]
fn publication_collision_preserves_staged_and_existing_files() -> anyhow::Result<()> {
    let (path, root) = super::tests::fixture()?;
    let (key, file, identity, _) = staged(&root)?;
    let target = format!("{}.mp4", uuid::Uuid::new_v4());
    std::fs::write(path.join(&target), b"unrelated")?;
    assert!(file.publish_staged(&target).is_err());
    assert_eq!(std::fs::read(path.join(&key))?, b"complete");
    assert_eq!(std::fs::read(path.join(target))?, b"unrelated");
    assert!(root.open_owned(&key, &identity, 8).is_ok());
    Ok(())
}

#[test]
fn publication_rejects_invalid_names_and_read_only_handles() -> anyhow::Result<()> {
    let (path, root) = super::tests::fixture()?;
    for target in ["../outside.mp4", "plain.mp4", "CON.mp4", "bad/name.mp4"] {
        let (key, file, _, _) = staged(&root)?;
        assert!(file.publish_staged(target).is_err());
        assert_eq!(std::fs::read(path.join(key))?, b"complete");
    }
    let (key, file, identity, _) = staged(&root)?;
    drop(file);
    let reopened = root.open_owned(&key, &identity, 8)?;
    let target = format!("{}.mp4", uuid::Uuid::new_v4());
    assert!(reopened.publish_staged(&target).is_err());
    assert!(!path.join(target).exists());
    assert_eq!(std::fs::read(path.join(key))?, b"complete");
    Ok(())
}

#[cfg(unix)]
#[test]
fn publication_rejects_replaced_root_and_preserves_its_contents() -> anyhow::Result<()> {
    let (path, root) = super::tests::fixture()?;
    let (key, file, _, _) = staged(&root)?;
    let retained = path.with_extension("retained");
    std::fs::rename(&path, &retained)?;
    std::fs::create_dir(&path)?;
    let target = format!("{}.mp4", uuid::Uuid::new_v4());
    assert!(file.publish_staged(&target).is_err());
    assert!(!path.join(target).exists());
    assert_eq!(std::fs::read(retained.join(key))?, b"complete");
    Ok(())
}
