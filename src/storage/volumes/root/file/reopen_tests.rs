use super::*;
use std::io::Write;

fn recording() -> anyhow::Result<(std::path::PathBuf, Root, String, String)> {
    let (path, root) = super::tests::fixture()?;
    let key = format!("{}.mp4", uuid::Uuid::new_v4());
    let mut file = root.create_file(&key)?;
    file.file_mut().write_all(b"retained")?;
    let (_, identity, _) = file.evidence()?;
    drop(file);
    Ok((path, root, key, identity))
}

#[test]
fn reopen_preserves_identity_and_allows_only_bounded_reading() -> anyhow::Result<()> {
    let (path, root, key, identity) = recording()?;
    let mut file = root.open_owned(&key, &identity, 8)?;
    let (_, observed, digest) = file.inspect_evidence()?;
    assert_eq!(observed, identity);
    assert_eq!(digest, <[u8; 32]>::from(Sha256::digest(b"retained")));
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)?;
    assert_eq!(contents, b"retained");
    assert!(file.file_mut().write_all(b"overwrite").is_err());
    assert!(file.file_mut().set_len(0).is_err());
    assert!(file.evidence().is_err());
    assert_eq!(std::fs::read(path.join(key))?, b"retained");
    Ok(())
}

#[test]
fn reopen_rejects_wrong_identity_size_missing_and_linked_files() -> anyhow::Result<()> {
    let (path, root, key, identity) = recording()?;
    assert!(root.open_owned(&key, "unrelated", 8).is_err());
    assert!(root.open_owned(&key, &identity, 7).is_err());
    assert!(root.open_owned(&key, &identity, 9).is_err());
    let missing = format!("{}.mp4", uuid::Uuid::new_v4());
    assert!(root.open_owned(&missing, &identity, 8).is_err());
    assert!(!path.join(missing).exists());
    std::fs::hard_link(path.join(&key), path.join("alias"))?;
    assert!(root.open_owned(&key, &identity, 8).is_err());
    std::fs::remove_file(path.join("alias"))?;
    std::fs::rename(path.join(&key), path.join("original"))?;
    std::fs::write(path.join(&key), b"replaced")?;
    assert!(root.open_owned(&key, &identity, 8).is_err());
    assert_eq!(std::fs::read(path.join(key))?, b"replaced");
    Ok(())
}

#[test]
fn reopened_reads_are_limited_to_one_chunk() -> anyhow::Result<()> {
    let (_, root) = super::tests::fixture()?;
    let key = format!("{}.mp4", uuid::Uuid::new_v4());
    let mut writer = root.create_file(&key)?;
    writer.file_mut().write_all(&vec![7; 131_072])?;
    let (bytes, identity, _) = writer.evidence()?;
    drop(writer);
    let mut reader = root.open_owned(&key, &identity, bytes)?;
    let mut buffer = vec![0; 131_072];
    assert_eq!(reader.read(&mut buffer)?, 65_536);
    assert!(buffer[..65_536].iter().all(|byte| *byte == 7));
    assert!(buffer[65_536..].iter().all(|byte| *byte == 0));
    Ok(())
}

#[test]
fn recovery_reopen_requires_identity_and_an_existing_length_inside_reservation()
-> anyhow::Result<()> {
    let (path, root) = super::tests::fixture()?;
    let key = format!("{}.tmp", uuid::Uuid::new_v4());
    let mut created = root.create_file(&key)?;
    created.file_mut().write_all(b"partial")?;
    let (_, identity, _) = created.evidence()?;
    drop(created);
    assert!(root.open_owned_writable(&key, "unrelated", 0, 16).is_err());
    assert!(root.open_owned_writable(&key, &identity, 8, 16).is_err());
    assert!(root.open_owned_writable(&key, &identity, 0, 6).is_err());
    assert!(root.open_owned_writable(&key, &identity, 16, 0).is_err());
    let mut reopened = root.open_owned_writable(&key, &identity, 0, 16)?;
    reopened.file_mut().seek(SeekFrom::End(0))?;
    reopened.file_mut().write_all(b" tail")?;
    assert_eq!(reopened.evidence()?.0, 12);
    drop(reopened);
    assert_eq!(std::fs::read(path.join(key))?, b"partial tail");
    let missing = format!("{}.tmp", uuid::Uuid::new_v4());
    assert!(
        root.open_owned_writable(&missing, &identity, 0, 16)
            .is_err()
    );
    assert!(!path.join(missing).exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn reopened_file_rejects_later_length_name_and_link_changes() -> anyhow::Result<()> {
    let (path, root, key, identity) = recording()?;
    let mut file = root.open_owned(&key, &identity, 8)?;
    std::fs::write(path.join(&key), b"short")?;
    assert!(file.read(&mut [0; 8]).is_err());
    assert!(file.inspect_evidence().is_err());
    std::fs::write(path.join(&key), b"retained")?;
    std::fs::rename(path.join(&key), path.join("original"))?;
    std::os::unix::fs::symlink(path.join("original"), path.join(&key))?;
    assert!(root.open_owned(&key, &identity, 8).is_err());
    assert!(file.read(&mut [0; 8]).is_err());
    Ok(())
}

#[cfg(windows)]
#[test]
fn reopened_file_prevents_external_mutation_and_replacement() -> anyhow::Result<()> {
    let (path, root, key, identity) = recording()?;
    let file = root.open_owned(&key, &identity, 8)?;
    assert!(std::fs::write(path.join(&key), b"changed!").is_err());
    assert!(std::fs::rename(path.join(&key), path.join("original")).is_err());
    drop(file);
    std::fs::rename(path.join(&key), path.join("original"))?;
    Ok(())
}
