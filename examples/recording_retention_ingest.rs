//! Measures real H.264 ingest and flush separately from archive sweeps.
use anyhow::Result;
#[path = "recording_retention_ingest/support.rs"]
mod support;

fn main() -> Result<()> {
    let enabled = match std::env::args().nth(1).as_deref() {
        Some("enabled") => true,
        Some("disabled") => false,
        _ => anyhow::bail!("use enabled or disabled"),
    };
    let root = std::env::temp_dir().join(format!("retention-ingest-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    std::thread::Builder::new()
        .name("retention-ingest".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let report = support::measure(&root, enabled, None)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            anyhow::Ok(())
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("ingest measurement panicked"))?
}
