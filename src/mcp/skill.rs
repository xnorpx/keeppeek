//! Bundled KeepPeek MCP agent skill rendering and file export.
//!
//! Operators can inspect the bundled `SKILL.md` document or write it into an
//! agent skills directory with `keeppeek mcp skill [--output <DIR>]`.

use std::path::{Path, PathBuf};

/// File name written by `keeppeek mcp skill --output <DIR>`.
pub const SKILL_FILE_NAME: &str = "SKILL.md";

// ponytail: Inline fallback markdown until the dedicated skill asset integrates via include_str!.
const SKILL_MARKDOWN: &str = r#"---
name: keeppeek-mcp
description: >-
  Inspect health, logs, and configuration on a KeepPeek server and apply bounded
  configuration changes through the embedded MCP endpoint.
---

# KeepPeek MCP Operator Skill

## Connection

Run the stdio bridge against a running KeepPeek server with `[mcp] enabled = true`:

```bash
KEEPPEEK_ACCESS_KEY="<uuid>" keeppeek mcp --server https://keeppeek.example.com
```

Add `--read-only` to prevent write tools for the entire session.

## Safe Workflow

1. Inspect server health (`get_server_health`), logging settings (`get_logging_settings`),
   and configuration (`get_configuration`).
2. List secret key names with `list_secret_keys`. Never pass raw passwords into config
   changes; reference existing secrets with `{secret:KEY}` or `{secret:KEY|url}`.
3. Always run `plan_configuration_change` first and inspect validation issues, targets,
   redacted diffs, and live-vs-restart impact.
4. Only call `apply_configuration_plan` after reviewing the plan and confirming writes
   are enabled (`mcp.allow_writes = true` and no `--read-only` flag).
"#;

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
    fn renders_and_writes_skill_markdown() {
        let markdown = render_skill_markdown();
        assert!(markdown.contains("name: keeppeek-mcp"));
        assert!(markdown.contains("plan_configuration_change"));

        let temp_dir =
            std::env::temp_dir().join(format!("keeppeek-mcp-skill-test-{}", uuid::Uuid::new_v4()));
        let path = write_skill_to_dir(&temp_dir).unwrap();
        assert_eq!(path, temp_dir.join(SKILL_FILE_NAME));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), markdown);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
