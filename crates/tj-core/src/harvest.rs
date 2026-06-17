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

use crate::artifacts::{ArtifactLink, Artifacts};
use std::path::Path;
use std::process::Command;

/// Raw git/gh outputs for one repo, before filtering. All optional and
/// best-effort — [`build`] decides what survives.
#[derive(Debug, Default, Clone)]
pub struct Raw {
    pub branch: Option<String>,
    pub commit_short: Option<String>,
    pub commit_full: Option<String>,
    pub pr_url: Option<String>,
    pub repo_url: Option<String>,
}

/// Pure assembler: turn raw git/gh outputs into a clean [`Artifacts`], dropping
/// the values that aren't real refs — a detached HEAD (`"HEAD"`), empty
/// strings, a non-http PR/repo line. Also emits clickable [`ArtifactLink`]s
/// when the repo web URL is known (so a commit hash becomes a real link).
/// Separated from the IO so the keep/drop rules are unit-testable without git.
pub fn build(raw: Raw) -> Artifacts {
    let mut a = Artifacts::default();

    let branch = raw
        .branch
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD");
    let commit_short = raw
        .commit_short
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    let commit_full = raw
        .commit_full
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    let pr_url = raw
        .pr_url
        .map(|u| u.trim().to_string())
        .filter(|u| u.starts_with("http"));
    let repo = raw
        .repo_url
        .map(|r| r.trim().trim_end_matches('/').to_string())
        .filter(|r| r.starts_with("http"));

    // Flat token vectors (power artifact search / relatedness, unchanged).
    if let Some(b) = &branch {
        a.branch_names.push(b.clone());
    }
    if let Some(c) = &commit_short {
        a.commit_hashes.push(c.clone());
    }
    if let Some(u) = &pr_url {
        a.pr_urls.push(u.clone());
    }

    // Clickable typed links for the card.
    if let Some(u) = &pr_url {
        let label = pr_number(u)
            .map(|n| format!("PR #{n}"))
            .unwrap_or_else(|| "PR".into());
        a.links.push(ArtifactLink {
            kind: "pr".into(),
            url: u.clone(),
            label,
        });
    }
    if let (Some(repo), Some(full), Some(short)) = (&repo, &commit_full, &commit_short) {
        a.links.push(ArtifactLink {
            kind: "commit".into(),
            url: format!("{repo}/commit/{full}"),
            label: short.clone(),
        });
    }
    if let (Some(repo), Some(b)) = (&repo, &branch) {
        a.links.push(ArtifactLink {
            kind: "branch".into(),
            url: format!("{repo}/tree/{b}"),
            label: b.clone(),
        });
    }
    a
}

/// Trailing PR/MR number from a GitHub/GitLab URL (`…/pull/54` → `54`).
fn pr_number(url: &str) -> Option<&str> {
    let tail = url.trim_end_matches('/').rsplit('/').next()?;
    if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
        Some(tail)
    } else {
        None
    }
}

/// Harvest commit/branch/PR refs from the git repo at `dir`. Best-effort;
/// returns an empty [`Artifacts`] when `dir` is not a repo or the tools are
/// absent. Used at task close to stamp deterministic refs onto the close event.
pub fn harvest(dir: &Path) -> Artifacts {
    let commit_full = git(dir, &["rev-parse", "HEAD"]);
    // PR resolution, best-effort and in order of reliability:
    //   1. the open PR for the current branch (pre-merge close), else
    //   2. the merged PR that contains HEAD (post-merge close, branch gone).
    let pr_url = gh_pr_url(dir).or_else(|| {
        commit_full
            .as_deref()
            .and_then(|sha| gh_pr_for_commit(dir, sha))
    });
    build(Raw {
        branch: git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
        commit_short: git(dir, &["rev-parse", "--short", "HEAD"]),
        commit_full,
        pr_url,
        repo_url: gh_repo_url(dir),
    })
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

/// Best-effort URL of the merged PR that introduced `sha`, via GitHub's commit
/// search. Used as a fallback when the branch's open PR is gone (task closed on
/// `main` after the branch was deleted). `None` on any failure.
fn gh_pr_for_commit(dir: &Path, sha: &str) -> Option<String> {
    let out = Command::new("gh")
        .args([
            "pr", "list", "--state", "merged", "--search", sha, "--limit", "1", "--json", "url",
            "-q", ".[0].url",
        ])
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

/// Best-effort web URL of the repo (`https://github.com/owner/repo`), used to
/// build clickable commit/branch links. `None` when `gh` is absent or the dir
/// is not a GitHub repo.
fn gh_repo_url(dir: &Path) -> Option<String> {
    let out = Command::new("gh")
        .args(["repo", "view", "--json", "url", "-q", ".url"])
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
    fn build_keeps_real_refs_and_emits_links() {
        let a = build(Raw {
            branch: Some("feat/clean-pack".into()),
            commit_short: Some("75f65e2".into()),
            commit_full: Some("75f65e2aaaa".into()),
            pr_url: Some("https://github.com/o/r/pull/51".into()),
            repo_url: Some("https://github.com/o/r".into()),
        });
        assert_eq!(a.branch_names, vec!["feat/clean-pack"]);
        assert_eq!(a.commit_hashes, vec!["75f65e2"]);
        assert_eq!(a.pr_urls, vec!["https://github.com/o/r/pull/51"]);
        // clickable links: PR (labelled by number), commit (full-sha url), branch
        let kinds: Vec<_> = a.links.iter().map(|l| l.kind.as_str()).collect();
        assert_eq!(kinds, vec!["pr", "commit", "branch"]);
        let pr = &a.links[0];
        assert_eq!(pr.label, "PR #51");
        assert_eq!(a.links[1].url, "https://github.com/o/r/commit/75f65e2aaaa");
        assert_eq!(
            a.links[2].url,
            "https://github.com/o/r/tree/feat/clean-pack"
        );
    }

    #[test]
    fn build_without_repo_url_keeps_flat_but_no_commit_branch_links() {
        // No repo URL → commit/branch can't be made clickable, but the PR URL
        // is self-sufficient so it still yields a link.
        let a = build(Raw {
            branch: Some("main".into()),
            commit_short: Some("abc1234".into()),
            commit_full: Some("abc1234ffff".into()),
            pr_url: Some("https://github.com/o/r/pull/9".into()),
            repo_url: None,
        });
        let kinds: Vec<_> = a.links.iter().map(|l| l.kind.as_str()).collect();
        assert_eq!(kinds, vec!["pr"], "only the self-linking PR survives");
        assert_eq!(a.commit_hashes, vec!["abc1234"]);
    }

    #[test]
    fn build_drops_detached_head_empty_and_non_http() {
        let a = build(Raw {
            branch: Some("HEAD".into()),
            commit_short: Some("  ".into()),
            commit_full: None,
            pr_url: Some("no pull request".into()),
            repo_url: Some("not-a-url".into()),
        });
        assert!(
            a.is_empty(),
            "detached HEAD + empty commit + non-url PR/repo all dropped"
        );
    }

    #[test]
    fn build_tolerates_all_absent() {
        assert!(build(Raw::default()).is_empty());
    }
}
