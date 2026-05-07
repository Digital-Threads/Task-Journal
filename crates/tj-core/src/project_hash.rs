use anyhow::Context;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Walk up from `start` to a project boundary so that running
/// `task-journal` from `repo/`, `repo/src/`, and `repo/src/foo/bar/`
/// all hash to the same project. Without this normalization, opening
/// Claude Code in a subdir gave an empty journal — broke the
/// "auto-memory" promise.
///
/// Boundary markers, priority order:
/// 1. `.task-journal/` directory — explicit opt-in for sub-projects
///    that intentionally want a separate journal from their parent.
/// 2. `.git` (file or directory) — covers normal checkouts and
///    worktrees alike (a worktree's root holds a `.git` *file*
///    pointing at the real gitdir, but its presence still marks the
///    boundary).
///
/// Falls back to `start` if no marker is found, preserving prior
/// behaviour for non-git scratch directories.
fn project_root(start: &Path) -> PathBuf {
    let mut cur = start;
    loop {
        if cur.join(".task-journal").is_dir() || cur.join(".git").exists() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return start.to_path_buf(),
        }
    }
}

pub fn from_path(p: impl AsRef<Path>) -> anyhow::Result<String> {
    let canonical = dunce::canonicalize(p.as_ref())
        .with_context(|| format!("canonicalize {:?}", p.as_ref()))?;
    let root = project_root(&canonical);
    let bytes = root.as_os_str().as_encoded_bytes();
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    debug_assert_eq!(hex.len(), 16);
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn same_path_yields_same_hash() {
        let d = TempDir::new().unwrap();
        let a = from_path(d.path()).unwrap();
        let b = from_path(d.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 16, "16 hex chars expected, got: {a}");
    }

    #[test]
    fn different_paths_yield_different_hashes() {
        let d1 = TempDir::new().unwrap();
        let d2 = TempDir::new().unwrap();
        let a = from_path(d1.path()).unwrap();
        let b = from_path(d2.path()).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn subdir_under_git_root_hashes_to_root() {
        // repo/ with .git inside; repo/src/foo/ should normalise to repo/.
        let repo = TempDir::new().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let sub = repo.path().join("src").join("foo");
        std::fs::create_dir_all(&sub).unwrap();

        let root_hash = from_path(repo.path()).unwrap();
        let sub_hash = from_path(&sub).unwrap();
        assert_eq!(
            root_hash, sub_hash,
            "subdir of a git repo must hash to the repo root, not the subdir"
        );
    }

    #[test]
    fn dot_task_journal_marker_overrides_git_boundary() {
        // repo/.git + repo/sub/.task-journal/. Then sub is its own project
        // (explicit opt-out of the parent's journal).
        let repo = TempDir::new().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let sub = repo.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::create_dir(sub.join(".task-journal")).unwrap();

        let root_hash = from_path(repo.path()).unwrap();
        let sub_hash = from_path(&sub).unwrap();
        assert_ne!(
            root_hash, sub_hash,
            "subdir with .task-journal/ marker must NOT inherit parent's project hash"
        );
    }

    #[test]
    fn dot_git_file_in_worktree_root_is_a_boundary() {
        // Worktrees have a `.git` *file* (not a dir) at their root.
        // We must still treat that as a boundary.
        let wt = TempDir::new().unwrap();
        std::fs::write(wt.path().join(".git"), "gitdir: /elsewhere\n").unwrap();
        let sub = wt.path().join("inner");
        std::fs::create_dir(&sub).unwrap();

        let wt_hash = from_path(wt.path()).unwrap();
        let sub_hash = from_path(&sub).unwrap();
        assert_eq!(
            wt_hash, sub_hash,
            "worktree subdir must normalise to worktree root via .git file"
        );
    }
}
