use anyhow::Context;
use std::path::PathBuf;

/// Base data directory for Task Journal on the current OS.
/// - Linux/WSL: $XDG_DATA_HOME/task-journal (default ~/.local/share/task-journal)
/// - macOS: ~/Library/Application Support/task-journal
/// - Windows: %LOCALAPPDATA%\task-journal
pub fn data_dir() -> anyhow::Result<PathBuf> {
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
}
