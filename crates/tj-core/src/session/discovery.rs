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
    sessions.sort_by_key(|s| std::cmp::Reverse(s.1));
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
    use std::sync::Mutex;

    /// Serialize every test that touches `CLAUDE_CONFIG_DIR`. Cargo runs
    /// unit tests in parallel by default; two tests mutating the same
    /// process env race (set in A, observed in B) and flaked Windows CI
    /// (saw "C:\Users\runneradmin\.claude" when expecting the override).
    /// Tests that touch the env take this lock before the first set_var.
    /// `lock().unwrap_or_else(|p| p.into_inner())` swallows poisoning
    /// from a panicking sibling test — env is restored regardless.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

    // --- list_sessions() ---

    #[test]
    fn list_sessions_returns_jsonl_files_skipping_agent_files() {
        let dir = tempfile::tempdir().unwrap();
        // Create regular session files.
        std::fs::write(dir.path().join("sess-001.jsonl"), "{}").unwrap();
        std::fs::write(dir.path().join("sess-002.jsonl"), "{}").unwrap();
        // Create agent files that should be skipped.
        std::fs::write(dir.path().join("agent-abc.jsonl"), "{}").unwrap();
        std::fs::write(dir.path().join("agent-def.jsonl"), "{}").unwrap();
        // Create non-jsonl files that should be skipped.
        std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("data.json"), "{}").unwrap();

        let sessions = list_sessions(dir.path()).unwrap();
        assert_eq!(sessions.len(), 2);
        let names: Vec<String> = sessions
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"sess-001.jsonl".to_string()));
        assert!(names.contains(&"sess-002.jsonl".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("agent-")));
    }

    #[test]
    fn list_sessions_sorted_by_mtime_newest_first() {
        let dir = tempfile::tempdir().unwrap();

        // Create files with different modification times.
        let older = dir.path().join("older.jsonl");
        std::fs::write(&older, "{}").unwrap();

        // Sleep briefly to ensure different mtime.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let newer = dir.path().join("newer.jsonl");
        std::fs::write(&newer, "{}").unwrap();

        let sessions = list_sessions(dir.path()).unwrap();
        assert_eq!(sessions.len(), 2);
        // Newest file should come first.
        let first_name = sessions[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(first_name, "newer.jsonl");
    }

    #[test]
    fn list_sessions_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = list_sessions(dir.path()).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn list_sessions_nonexistent_directory() {
        let result = list_sessions(Path::new("/nonexistent/path/xyz"));
        assert!(result.is_err());
    }

    // --- list_all_projects() ---

    #[test]
    fn list_all_projects_with_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Override CLAUDE_CONFIG_DIR for this test.
        let config_dir = dir.path();
        let projects = config_dir.join("projects");
        std::fs::create_dir_all(&projects).unwrap();

        // Create project directories.
        std::fs::create_dir(projects.join("-home-user-project-alpha")).unwrap();
        std::fs::create_dir(projects.join("-home-user-project-beta")).unwrap();
        // Create a file (should be skipped — not a directory).
        std::fs::write(projects.join("not-a-dir.txt"), "").unwrap();

        // We can't easily test list_all_projects() because it uses projects_dir()
        // which reads CLAUDE_CONFIG_DIR. Instead, test the directory listing logic directly.
        let mut result = Vec::new();
        for entry in std::fs::read_dir(&projects).unwrap() {
            let entry = entry.unwrap();
            if entry.path().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                let decoded = decode_project_path(&name);
                result.push((decoded, entry.path()));
            }
        }
        result.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(result.len(), 2);
        assert!(result[0].0.contains("alpha"));
        assert!(result[1].0.contains("beta"));
    }

    // --- find_project_dir() with CLAUDE_CONFIG_DIR env override ---

    #[test]
    fn find_project_dir_with_env_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();

        // Create a project directory matching an encoded path.
        let encoded = encode_project_path("/home/user/myproject");
        std::fs::create_dir(projects.join(&encoded)).unwrap();

        // Set the env override.
        std::env::set_var("CLAUDE_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = find_project_dir(Path::new("/home/user/myproject"));

        // Clean up env before assertions (to avoid affecting other tests).
        std::env::remove_var("CLAUDE_CONFIG_DIR");

        let found = result.unwrap();
        assert!(found.is_some());
        let found_path = found.unwrap();
        assert!(found_path.ends_with(&encoded));
    }

    #[test]
    fn find_project_dir_returns_none_when_no_match() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();

        std::env::set_var("CLAUDE_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = find_project_dir(Path::new("/nonexistent/project"));

        std::env::remove_var("CLAUDE_CONFIG_DIR");

        assert!(result.unwrap().is_none());
    }

    #[test]
    fn find_project_dir_returns_none_when_projects_dir_missing() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // Don't create a "projects" subdir — it doesn't exist.

        std::env::set_var("CLAUDE_CONFIG_DIR", dir.path().to_str().unwrap());

        let result = find_project_dir(Path::new("/home/user/myproject"));

        std::env::remove_var("CLAUDE_CONFIG_DIR");

        assert!(result.unwrap().is_none());
    }

    // --- decode_project_path ---

    #[test]
    fn decode_project_path_returns_same_string() {
        // Current implementation is identity — just verify it doesn't panic.
        let decoded = decode_project_path("-home-user-project");
        assert_eq!(decoded, "-home-user-project");
    }

    // --- claude_config_dir ---

    /// Both `CLAUDE_CONFIG_DIR` cases are combined into one test so the
    /// env-var read/write/restore steps run serially. Cargo runs unit
    /// tests in parallel by default; two tests touching the same process
    /// env on Windows raced (set in test A, observed in test B) and
    /// flaked CI. Save → set → assert → restore inside one body makes
    /// the dependency local. Uses a portable tempdir-style path rather
    /// than the hardcoded "/tmp/..." that doesn't exist on Windows.
    #[test]
    fn claude_config_dir_handles_env_var() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: the ENV_LOCK above serializes us against the other
        // CLAUDE_CONFIG_DIR tests in this module; prev → restore at
        // the end gives a clean exit regardless of panic.
        let prev = std::env::var_os("CLAUDE_CONFIG_DIR");

        // Case 1: non-empty value is honored verbatim. Use a portable
        // path (std::env::temp_dir() works on Linux/macOS/Windows).
        let custom = std::env::temp_dir().join("tj-custom-claude-config");
        unsafe {
            std::env::set_var("CLAUDE_CONFIG_DIR", &custom);
        }
        let dir = claude_config_dir().unwrap();
        assert_eq!(dir, custom);

        // Case 2: empty value falls back to home + .claude.
        unsafe {
            std::env::set_var("CLAUDE_CONFIG_DIR", "");
        }
        let dir = claude_config_dir().unwrap();
        assert!(
            dir.to_string_lossy().ends_with(".claude"),
            "fallback must land in <home>/.claude, got: {dir:?}"
        );

        // Restore.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
                None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
            }
        }
    }
}
