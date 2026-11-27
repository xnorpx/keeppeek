//! Bundled KeepPeek MCP agent skill rendering and file export.
//!
//! Operators can inspect the bundled `SKILL.md` document or write it into an
//! agent skills directory with `keeppeek mcp skill [--output <DIR>]`.

use std::path::{Path, PathBuf};

/// File name written by `keeppeek mcp skill --output <DIR>`.
pub const SKILL_FILE_NAME: &str = "SKILL.md";

const SKILL_MARKDOWN: &str = include_str!("../../skills/keeppeek-mcp/SKILL.md");

/// Returns the bundled KeepPeek MCP skill document.
#[must_use]
pub const fn render_skill_markdown() -> &'static str {
    SKILL_MARKDOWN
}

/// Writes `SKILL.md` into `output_dir`, creating parent directories as needed.
///
/// # Errors
///
/// Returns an error when the directory cannot be created or the file cannot be written.
pub fn write_skill_to_dir(output_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(output_dir)?;
    let destination = output_dir.join(SKILL_FILE_NAME);
    std::fs::write(&destination, render_skill_markdown().as_bytes())?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_writes_canonical_skill_markdown() {
        let markdown = render_skill_markdown();
        assert!(markdown.starts_with("---\nname: keeppeek-mcp\n"));
        assert!(markdown.contains("plan_configuration_change"));
        assert!(markdown.contains("get_camera_snapshot"));

        let canonical_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("skills")
            .join("keeppeek-mcp")
            .join(SKILL_FILE_NAME);
        let canonical = std::fs::read_to_string(&canonical_path).unwrap();
        assert_eq!(markdown, canonical);

        let temp_dir =
            std::env::temp_dir().join(format!("keeppeek-mcp-skill-test-{}", uuid::Uuid::new_v4()));
        let path = write_skill_to_dir(&temp_dir).unwrap();
        assert_eq!(path, temp_dir.join(SKILL_FILE_NAME));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), markdown);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
