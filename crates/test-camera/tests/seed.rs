use keeppeek::{
    config,
    storage::{
        RecordingCatalog, StorageConfig,
        catalog::locations::{Reply, Request},
    },
};
use std::{fs, process::Command};

#[test]
fn configured_seed_uses_named_ownership_and_preserves_an_offline_metadata_root()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().to_path_buf();
    #[cfg(unix)]
    let root = root.canonicalize()?;
    let path = root.join("config.toml");
    config::write_private_file(&path, b"[storage]\nlong_term_max_gb=0\n")?;
    let result = seed(&path)?;
    anyhow::ensure!(
        result.status.success(),
        "configured seed failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let settings = config::load_config(&path)?;
    let storage = StorageConfig::from_toml(&settings.storage);
    let binding = storage.metadata.as_ref().unwrap();
    let catalog = RecordingCatalog::open_managed(
        &storage.recording_catalog_path,
        &binding.authority(),
        &binding.root_identity(),
    )?;
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        panic!("missing volume usage");
    };
    let media = usage.iter().find(|item| item.volume == "media").unwrap();
    assert!(media.allocated_bytes > 0);
    assert_eq!(media.reserved_bytes, 0);
    let files = fs::read_dir(root.join("storage/media"))?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(files.len(), 1);
    let file = files[0].path();
    assert_eq!(fs::metadata(&file)?.len(), media.allocated_bytes);
    let reader = mp4::read_mp4(fs::File::open(file)?)?;
    assert!(
        reader
            .tracks()
            .values()
            .any(|track| track.sample_count() > 0)
    );
    drop(reader);
    catalog.shutdown();
    let metadata = root.join("storage/metadata");
    let offline = root.join("offline-metadata");
    fs::rename(&metadata, &offline)?;
    assert!(!seed(&path)?.status.success());
    assert!(!metadata.exists());
    fs::rename(offline, metadata)?;
    Ok(())
}

fn seed(path: &std::path::Path) -> std::io::Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_test_camera"))
        .arg("seed-recording")
        .arg("--config")
        .arg(path)
        .args([
            "--source",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/testdata/cc-4k-640x360-h264.mp4"
            ),
            "--stream-id",
            "camera/main",
            "--duration-seconds",
            "1",
            "--age-seconds",
            "120",
        ])
        .output()
}
