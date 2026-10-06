//! Measures real H.264 ingest and flush separately from archive sweeps.
use anyhow::{Result, ensure};
#[path = "recording_retention_ingest/support.rs"]
mod support;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| anyhow::anyhow!("benchmark tracing initialization failed: {error}"))?;
    let enabled = match std::env::args().nth(1).as_deref() {
        Some("enabled") => true,
        Some("disabled") => false,
        _ => anyhow::bail!("use enabled or disabled"),
    };
    let existing = std::env::args().nth(2).map(std::path::PathBuf::from);
    if let Some(path) = &existing {
        let path = path.canonicalize()?;
        ensure!(
            path.is_file()
                && path.starts_with(std::env::temp_dir().canonicalize()?)
                && path
                    .parent()
                    .and_then(std::path::Path::file_name)
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|name| name.starts_with("retention-scale-")),
            "existing catalog must be a closed generated retention-scale fixture in the temporary directory"
        );
    }
    let root = std::env::temp_dir().join(format!("retention-ingest-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    std::thread::Builder::new()
        .name("retention-ingest".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let report = support::measure(&root, enabled, existing.as_deref(), 35)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            anyhow::Ok(())
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("ingest measurement panicked"))?
}
