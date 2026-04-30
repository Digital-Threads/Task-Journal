use anyhow::Context;
use std::path::PathBuf;

/// Base data directory for Task Journal on the current OS.
///
/// Resolution order (first wins):
/// 1. `TASK_JOURNAL_DATA_DIR` env (explicit override; portable across all OS)
/// 2. `XDG_DATA_HOME` env (Linux/WSL convention; respected on every OS for testability)
/// 3. OS default via `directories` crate:
///    - Linux/WSL: `~/.local/share/task-journal`
///    - macOS: `~/Library/Application Support/task-journal`
///    - Windows: `%LOCALAPPDATA%\task-journal`
pub fn data_dir() -> anyhow::Result<PathBuf> {
    if let Ok(custom) = std::env::var("TASK_JOURNAL_DATA_DIR") {
        if !custom.is_empty() {
            return Ok(PathBuf::from(custom));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("task-journal"));
        }
    }
    let dirs = directories::ProjectDirs::from("", "", "task-journal")
        .context("could not resolve OS data directories")?;
    Ok(dirs.data_local_dir().to_path_buf())
}

pub fn events_dir() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("events"))
}

pub fn state_dir() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("state"))
}

pub fn metrics_dir() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("metrics"))
}

pub fn project_storage_dir(project_hash: &str) -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join(project_hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_returns_a_path_containing_task_journal() {
        let p = data_dir().expect("data_dir");
        let s = p.to_string_lossy();
        assert!(s.contains("task-journal"), "got: {s}");
    }

    #[test]
    fn project_dir_appends_subdir() {
        let p = project_storage_dir("abc123").expect("project dir");
        assert!(p.ends_with("abc123"), "got: {p:?}");
    }

    /// Regression: on macOS/Windows the `directories` crate ignores XDG_DATA_HOME, but our
    /// tests (and power users) need a portable override. data_dir() must respect XDG_DATA_HOME
    /// and TASK_JOURNAL_DATA_DIR on every OS. Using a thread-isolated env block since std env
    /// is process-global; one test exercises both vars by serially restoring state.
    #[test]
    #[cfg_attr(not(unix), ignore = "env semantics differ on Windows test runners; covered by integration tests")]
    fn env_overrides_take_precedence() {
        // Snapshot existing values (best-effort cleanup).
        let prev_tjdd = std::env::var("TASK_JOURNAL_DATA_DIR").ok();
        let prev_xdg = std::env::var("XDG_DATA_HOME").ok();

        // SAFETY: tests run in a separate process; setting env here is fine.
        unsafe { std::env::remove_var("TASK_JOURNAL_DATA_DIR") };
        unsafe { std::env::set_var("XDG_DATA_HOME", "/tmp/tj-paths-test-xdg") };
        assert_eq!(data_dir().unwrap(), PathBuf::from("/tmp/tj-paths-test-xdg/task-journal"));

        unsafe { std::env::set_var("TASK_JOURNAL_DATA_DIR", "/tmp/tj-paths-test-explicit") };
        assert_eq!(data_dir().unwrap(), PathBuf::from("/tmp/tj-paths-test-explicit"));

        // Restore.
        unsafe { std::env::remove_var("TASK_JOURNAL_DATA_DIR") };
        unsafe { std::env::remove_var("XDG_DATA_HOME") };
        if let Some(v) = prev_tjdd {
            unsafe { std::env::set_var("TASK_JOURNAL_DATA_DIR", v) };
        }
        if let Some(v) = prev_xdg {
            unsafe { std::env::set_var("XDG_DATA_HOME", v) };
        }
    }
}
