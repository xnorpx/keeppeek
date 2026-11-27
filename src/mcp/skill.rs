//! Bundled KeepPeek MCP agent skill rendering and file export.
//!
//! Operators can list, inspect, or export bundled `SKILL.md` documents with
//! `keeppeek mcp skill [--list] [<SKILL>] [--output <DIR>]`.

use std::path::{Path, PathBuf};

/// File name written by `keeppeek mcp skill --output <DIR>`.
pub const SKILL_FILE_NAME: &str = "SKILL.md";

const KEEPPEEK_MCP_MARKDOWN: &str = include_str!("../../skills/keeppeek-mcp/SKILL.md");
const KEEPPEEK_CAMERA_DEBUG_MARKDOWN: &str =
    include_str!("../../skills/keeppeek-camera-debug/SKILL.md");

/// Identifier for a bundled KeepPeek agent skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundledSkill {
    /// Primary KeepPeek setup, onboarding, configuration, and health skill.
    KeeppeekMcp,
    /// Camera protocol debugging, PCAPNG capture, and offline replay skill.
    KeeppeekCameraDebug,
}

const ALL_SKILLS: [BundledSkill; 2] =
    [BundledSkill::KeeppeekMcp, BundledSkill::KeeppeekCameraDebug];

impl BundledSkill {
    /// Returns the canonical skill name used in frontmatter and directories.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::KeeppeekMcp => "keeppeek-mcp",
            Self::KeeppeekCameraDebug => "keeppeek-camera-debug",
        }
    }

    /// Returns a concise one-line description of the bundled skill.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::KeeppeekMcp => {
                "Set up KeepPeek MCP, onboard cameras without plaintext secrets, and inspect health or logs."
            }
            Self::KeeppeekCameraDebug => {
                "Capture decrypted camera traffic and bitstreams via MCP and reproduce fixes offline with ReplayCameraServer."
            }
        }
    }
}

/// Returns every bundled KeepPeek agent skill in display order.
#[must_use]
pub const fn list_skills() -> &'static [BundledSkill] {
    &ALL_SKILLS
}

/// Resolves a skill name or supported alias into a [`BundledSkill`].
///
/// # Errors
///
/// Returns an error when `name` is empty or does not match a bundled skill.
pub fn resolve_skill(name: &str) -> anyhow::Result<BundledSkill> {
    let normalized = name.trim();
    match normalized {
        "keeppeek-mcp" => Ok(BundledSkill::KeeppeekMcp),
        "keeppeek-camera-debug" | "camera-debug" => Ok(BundledSkill::KeeppeekCameraDebug),
        "" => anyhow::bail!(
            "bundled skill name cannot be empty; run `keeppeek mcp skill --list` to inspect available skills"
        ),
        unknown => anyhow::bail!(
            "unknown bundled skill `{unknown}`; run `keeppeek mcp skill --list` to inspect available skills"
        ),
    }
}

/// Returns the canonical markdown document for the requested [`BundledSkill`].
#[must_use]
pub const fn render_named_skill_markdown(skill: BundledSkill) -> &'static str {
    match skill {
        BundledSkill::KeeppeekMcp => KEEPPEEK_MCP_MARKDOWN,
        BundledSkill::KeeppeekCameraDebug => KEEPPEEK_CAMERA_DEBUG_MARKDOWN,
    }
}

/// Writes `SKILL.md` for `skill` into `output_dir` and returns the file path.
///
/// # Errors
///
/// Returns an error when the directory cannot be created or the file cannot be written.
pub fn write_named_skill_to_dir(skill: BundledSkill, output_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(output_dir)?;
    let destination = output_dir.join(SKILL_FILE_NAME);
    std::fs::write(&destination, render_named_skill_markdown(skill).as_bytes())?;
    Ok(destination)
}

/// Returns the bundled `keeppeek-mcp` skill document.
#[must_use]
pub const fn render_skill_markdown() -> &'static str {
    render_named_skill_markdown(BundledSkill::KeeppeekMcp)
}

/// Writes `SKILL.md` for `keeppeek-mcp` into `output_dir`, creating parent directories as needed.
///
/// # Errors
///
/// Returns an error when the directory cannot be created or the file cannot be written.
pub fn write_skill_to_dir(output_dir: &Path) -> anyhow::Result<PathBuf> {
    write_named_skill_to_dir(BundledSkill::KeeppeekMcp, output_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_writes_canonical_mcp_skill_markdown() {
        let markdown = render_skill_markdown();
        assert!(markdown.starts_with("---\nname: keeppeek-mcp\n"));
        assert!(markdown.contains("plan_configuration_change"));
        assert!(markdown.contains("get_camera_snapshot"));
        assert_eq!(
            markdown,
            render_named_skill_markdown(BundledSkill::KeeppeekMcp)
        );

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

    #[test]
    fn resolves_lists_and_writes_camera_debug_skill_markdown() {
        let skills = list_skills();
        assert_eq!(
            skills,
            &[BundledSkill::KeeppeekMcp, BundledSkill::KeeppeekCameraDebug]
        );
        for skill in skills {
            assert!(!skill.name().is_empty());
            assert!(!skill.description().contains('\n'));
        }

        assert_eq!(
            resolve_skill("keeppeek-mcp").unwrap(),
            BundledSkill::KeeppeekMcp
        );
        assert_eq!(
            resolve_skill("keeppeek-camera-debug").unwrap(),
            BundledSkill::KeeppeekCameraDebug
        );
        assert_eq!(
            resolve_skill(" camera-debug ").unwrap(),
            BundledSkill::KeeppeekCameraDebug
        );
        assert!(resolve_skill("").is_err());
        assert!(resolve_skill("unknown-skill").is_err());

        let markdown = render_named_skill_markdown(BundledSkill::KeeppeekCameraDebug);
        assert!(markdown.starts_with("---\nname: keeppeek-camera-debug\n"));
        assert!(markdown.contains("allow_camera_debugging"));
        assert!(markdown.contains("ReplayCameraServer"));

        let canonical_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("skills")
            .join("keeppeek-camera-debug")
            .join(SKILL_FILE_NAME);
        let canonical = std::fs::read_to_string(&canonical_path).unwrap();
        assert_eq!(markdown, canonical);

        let temp_dir = std::env::temp_dir().join(format!(
            "keeppeek-camera-debug-skill-test-{}",
            uuid::Uuid::new_v4()
        ));
        let path = write_named_skill_to_dir(BundledSkill::KeeppeekCameraDebug, &temp_dir).unwrap();
        assert_eq!(path, temp_dir.join(SKILL_FILE_NAME));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), markdown);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
