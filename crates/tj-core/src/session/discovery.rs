//! Discover Claude Code session JSONL files for a project.
//!
//! Sessions live at `~/.claude/projects/<encoded-path>/<uuid>.jsonl`.
//! The encoded path replaces non-alphanumeric chars (except `-`) with `-`.

use std::path::{Path, PathBuf};

/// Resolve the Claude Code config directory.
/// Uses `CLAUDE_CONFIG_DIR` env if set, otherwise `~/.claude`.
pub fn claude_config_dir() -> anyhow::Result<PathBuf> {
    if let Ok(custom) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !custom.is_empty() {
            return Ok(PathBuf::from(custom));
        }
    }
    let home = dirs_home()?;
    Ok(home.join(".claude"))
}

/// Get the projects directory where session files live.
pub fn projects_dir() -> anyhow::Result<PathBuf> {
    Ok(claude_config_dir()?.join("projects"))
}

/// Encode a filesystem path into the Claude Code project directory name format.
/// Non-alphanumeric chars (except `-`) are replaced with `-`.
pub fn encode_project_path(path: &str) -> String {
    path.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Find the project directory for a given filesystem path.
/// Tries exact match first, then prefix match for worktree variants.
pub fn find_project_dir(project_path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let projects = projects_dir()?;
    if !projects.exists() {
        return Ok(None);
    }

    let encoded = encode_project_path(&project_path.to_string_lossy());

    // Try exact match first.
    let exact = projects.join(&encoded);
    if exact.is_dir() {
        return Ok(Some(exact));
    }

    // Try case-insensitive match (WSL paths can differ in case).
    let encoded_lower = encoded.to_lowercase();
    if let Ok(entries) = std::fs::read_dir(&projects) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.to_lowercase() == encoded_lower && entry.path().is_dir() {
                return Ok(Some(entry.path()));
            }
        }
    }

    Ok(None)
}

/// List all session JSONL files in a project directory.
/// Excludes agent files (starting with `agent-`).
/// Returns files sorted by modification time (newest first).
pub fn list_sessions(project_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut sessions: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();

    for entry in std::fs::read_dir(project_dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if !name.ends_with(".jsonl") {
            continue;
        }
        // Skip agent sessions.
        if name.starts_with("agent-") {
            continue;
        }

        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);

        sessions.push((path, mtime));
    }

    // Sort newest first.
    sessions.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(sessions.into_iter().map(|(p, _)| p).collect())
}

/// List all project directories in Claude Code config.
pub fn list_all_projects() -> anyhow::Result<Vec<(String, PathBuf)>> {
    let projects = projects_dir()?;
    if !projects.exists() {
        return Ok(vec![]);
    }

    let mut result = Vec::new();
    for entry in std::fs::read_dir(&projects)? {
        let entry = entry?;
        if entry.path().is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Decode the project name back to a readable path.
            let decoded = decode_project_path(&name);
            result.push((decoded, entry.path()));
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

/// Decode an encoded project directory name back to a readable path.
/// This is approximate — we can't distinguish `-` from original `/`.
fn decode_project_path(encoded: &str) -> String {
    // Common pattern: leading `--` means the path started with a path separator.
    // Replace double dashes carefully.
    encoded.to_string()
}

fn dirs_home() -> anyhow::Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_path_replaces_separators() {
        let encoded = encode_project_path("/home/user/project");
        assert_eq!(encoded, "-home-user-project");
    }

    #[test]
    fn encode_preserves_dashes() {
        let encoded = encode_project_path("/home/my-project");
        assert_eq!(encoded, "-home-my-project");
    }

    #[test]
    fn encode_wsl_path() {
        let encoded = encode_project_path("\\\\wsl.localhost\\ubuntu\\home\\user\\project");
        assert_eq!(encoded, "--wsl-localhost-ubuntu-home-user-project");
    }
}
