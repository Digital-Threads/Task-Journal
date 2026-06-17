//! Deterministic close-time artifact harvest.
//!
//! When a task closes we want its resume pack to carry the *real* refs of what
//! shipped — the commit, the branch, the PR — as structured [`Artifacts`], not
//! as hopeful regex scrapes of free-form prose. This module shells out to
//! `git`/`gh` in the task's repo and returns whatever it can find.
//!
//! Strictly best-effort and side-effect free: a missing repo, a detached HEAD,
//! an absent `gh`, or no PR for the branch simply yields fewer artifacts. It
//! NEVER errors and NEVER runs a model — this is the cheap, deterministic
//! Layer-2 of the "perfect pack at close" design. The pure [`build`] decides
//! what to keep so the filtering is unit-testable without a live repo.

use crate::artifacts::Artifacts;
use std::path::Path;
use std::process::Command;

/// Pure assembler: turn the raw `(branch, commit, pr_url)` git/gh outputs into
/// a clean [`Artifacts`], dropping the values that aren't real refs — a
/// detached HEAD (`"HEAD"`), empty strings, or a non-http PR line. Separated
/// from the IO so the keep/drop rules can be tested without spawning git.
pub fn build(branch: Option<String>, commit: Option<String>, pr_url: Option<String>) -> Artifacts {
    let mut a = Artifacts::default();
    if let Some(b) = branch {
        let b = b.trim();
        // "HEAD" means detached — not a branch name worth recording.
        if !b.is_empty() && b != "HEAD" {
            a.branch_names.push(b.to_string());
        }
    }
    if let Some(c) = commit {
        let c = c.trim();
        if !c.is_empty() {
            a.commit_hashes.push(c.to_string());
        }
    }
    if let Some(u) = pr_url {
        let u = u.trim();
        if u.starts_with("http") {
            a.pr_urls.push(u.to_string());
        }
    }
    a
}

/// Harvest commit/branch/PR refs from the git repo at `dir`. Best-effort;
/// returns an empty [`Artifacts`] when `dir` is not a repo or the tools are
/// absent. Used at task close to stamp deterministic refs onto the close event.
pub fn harvest(dir: &Path) -> Artifacts {
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let commit = git(dir, &["rev-parse", "--short", "HEAD"]);
    let pr_url = gh_pr_url(dir);
    build(branch, commit, pr_url)
}

/// Run `git -C <dir> <args>` and return trimmed stdout, or `None` on any
/// failure (missing git, not a repo, non-zero exit).
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Best-effort PR URL for the repo's current branch via `gh`. `None` when `gh`
/// is absent, unauthenticated, or the branch has no PR. May make a network
/// call, so it is the slowest part of the harvest — still bounded to one
/// short-lived child and never blocks the close on failure.
fn gh_pr_url(dir: &Path) -> Option<String> {
    let out = Command::new("gh")
        .args(["pr", "view", "--json", "url", "-q", ".url"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.starts_with("http") {
        Some(s)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_keeps_real_refs() {
        let a = build(
            Some("feat/clean-pack".into()),
            Some("75f65e2".into()),
            Some("https://github.com/o/r/pull/51".into()),
        );
        assert_eq!(a.branch_names, vec!["feat/clean-pack"]);
        assert_eq!(a.commit_hashes, vec!["75f65e2"]);
        assert_eq!(a.pr_urls, vec!["https://github.com/o/r/pull/51"]);
    }

    #[test]
    fn build_drops_detached_head_empty_and_non_http() {
        let a = build(
            Some("HEAD".into()),
            Some("  ".into()),
            Some("no pull request".into()),
        );
        assert!(
            a.is_empty(),
            "detached HEAD + empty commit + non-url PR all dropped"
        );
    }

    #[test]
    fn build_tolerates_all_absent() {
        assert!(build(None, None, None).is_empty());
    }
}
