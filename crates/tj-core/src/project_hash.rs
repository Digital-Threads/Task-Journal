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
///    boundary). Like git itself, a `.git` directory counts only when it
///    holds `HEAD`: Codex's sandbox (bubblewrap) briefly mounts an empty
///    `.git` into a writable root such as `/tmp` while a command runs.
///
/// Falls back to `start` if no marker is found, preserving prior
/// behaviour for non-git scratch directories.
pub fn project_root(start: &Path) -> PathBuf {
    let mut cur = start;
    loop {
        let git = cur.join(".git");
        if cur.join(".task-journal").is_dir() || git.is_file() || git.join("HEAD").exists() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return start.to_path_buf(),
        }
    }
}

/// The main checkout of the repository a linked git worktree belongs to,
/// where the project chronicle (the module map) lives. `None` when `dir` is
/// not in a linked worktree: the project is its own home. A worktree's root
/// holds a `.git` file naming its gitdir, whose `commondir` leads to the
/// main repository's `.git`; a submodule's gitdir has no `commondir`.
pub fn chronicle_home(dir: &Path) -> Option<PathBuf> {
    let dir = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let root = project_root(&dir);
    let pointer = std::fs::read_to_string(root.join(".git")).ok()?;
    let gitdir = root.join(pointer.trim().strip_prefix("gitdir:")?.trim());
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = dunce::canonicalize(gitdir.join(common.trim())).ok()?;

    match common.file_name() {
        Some(name) if name == ".git" => common.parent().map(Path::to_path_buf),
        _ => None,
    }
}

pub fn from_path(p: impl AsRef<Path>) -> anyhow::Result<String> {
    let p = p.as_ref();
    // `canonicalize` requires the path to EXIST — it returns ENOENT ("No such
    // file or directory") otherwise. A Loom task session can resolve its
    // project dir before the worktree is checked out, which made `task_create`
    // hard-fail on its very first path resolution. When the path is merely
    // absent (not a permission/other error), fall back to a lexical
    // absolutisation that touches no filesystem, so journal resolution still
    // works instead of erroring.
    let canonical = match dunce::canonicalize(p) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
        }
        Err(e) => return Err(e).with_context(|| format!("canonicalize {p:?}")),
    };
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
    fn nonexistent_path_falls_back_instead_of_erroring() {
        // Regression: `from_path` used to ENOENT on a path that doesn't exist
        // yet (canonicalize requires existence), which made task_create
        // hard-fail in a Loom session whose worktree wasn't checked out.
        let base = TempDir::new().unwrap();
        let missing = base.path().join("not/created/yet");
        let h = from_path(&missing).expect("must not fail on a missing path");
        assert_eq!(h.len(), 16);
        // Deterministic for the same absent path.
        assert_eq!(h, from_path(&missing).unwrap());
    }

    #[test]
    fn different_paths_yield_different_hashes() {
        let d1 = TempDir::new().unwrap();
        let d2 = TempDir::new().unwrap();
        // Make each temp dir its own project root, so the hash is of the temp
        // dir itself — not of a shared ancestor that happens to carry a `.git`
        // (which collapses both to one hash on some machines, e.g. WSL /tmp).
        std::fs::create_dir(d1.path().join(".git")).unwrap();
        std::fs::create_dir(d2.path().join(".git")).unwrap();
        let a = from_path(d1.path()).unwrap();
        let b = from_path(d2.path()).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn subdir_under_git_root_hashes_to_root() {
        // repo/ with .git inside; repo/src/foo/ should normalise to repo/.
        let repo = TempDir::new().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        std::fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
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
        std::fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
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
    fn a_worktree_finds_the_main_checkout_as_its_chronicle_home() {
        let d = tempfile::TempDir::new().unwrap();
        let main = d.path().join("repo");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(&main)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        std::fs::create_dir_all(main.join("src")).unwrap();
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ]);
        let wt = d.path().join("wt");
        git(&["worktree", "add", "-q", wt.to_str().unwrap()]);
        std::fs::create_dir_all(wt.join("deep/dir")).unwrap();

        let main_root = dunce::canonicalize(&main).unwrap();
        assert_eq!(chronicle_home(&wt.join("deep/dir")), Some(main_root));
        assert_eq!(chronicle_home(&main.join("src")), None);
    }

    #[test]
    fn a_git_file_without_a_common_dir_is_its_own_home() {
        // A submodule's `.git` file points into the parent's modules dir,
        // which has no `commondir`: it is a project of its own.
        let d = tempfile::TempDir::new().unwrap();
        let gitdir = d.path().join("parent/.git/modules/sub");
        std::fs::create_dir_all(&gitdir).unwrap();
        let sub = d.path().join("parent/sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();

        assert_eq!(chronicle_home(&sub), None);
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

    #[test]
    fn empty_dot_git_dir_is_not_a_boundary() {
        // Codex's bubblewrap sandbox briefly mounts an empty `.git` into a
        // writable root such as `/tmp` while a command runs. Git does not take
        // it for a repository, and the project hash must not flip meanwhile.
        let base = TempDir::new().unwrap();
        let proj = base.path().join("proj");
        std::fs::create_dir(&proj).unwrap();
        let before = from_path(&proj).unwrap();

        std::fs::create_dir(base.path().join(".git")).unwrap();
        assert_eq!(from_path(&proj).unwrap(), before);
    }
}
