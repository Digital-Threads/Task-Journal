//! Artifact extraction — regex-based scrape of structured references
//! out of free-form event text. Captures the bits that turn a journal
//! entry into a real ledger of what shipped: commit hashes, PR URLs,
//! ticket IDs, branch names, file paths.
//!
//! Intentionally regex-only and side-effect free: the classifier may
//! still emit a richer JSON payload in the future, but those will be
//! merged into the same shape via `Artifacts::merge`. Keeping the
//! extractor pure means `reclassify` can run it offline over historic
//! events without spawning the model.

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Structured artifacts collected from one or many events. All vectors
/// are deduplicated (case-sensitive) by the `merge` constructor — the
/// extractor itself emits raw matches.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifacts {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commit_hashes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pr_urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_issues: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branch_names: Vec<String>,
}

impl Artifacts {
    pub fn is_empty(&self) -> bool {
        self.commit_hashes.is_empty()
            && self.pr_urls.is_empty()
            && self.linked_issues.is_empty()
            && self.files.is_empty()
            && self.branch_names.is_empty()
    }

    /// Merge another `Artifacts` into self, preserving insertion order
    /// and deduplicating exact-match strings.
    pub fn merge(&mut self, other: Artifacts) {
        for (dst, src) in [
            (&mut self.commit_hashes, other.commit_hashes),
            (&mut self.pr_urls, other.pr_urls),
            (&mut self.linked_issues, other.linked_issues),
            (&mut self.files, other.files),
            (&mut self.branch_names, other.branch_names),
        ] {
            for s in src {
                if !dst.iter().any(|x| x == &s) {
                    dst.push(s);
                }
            }
        }
    }
}

/// Extract artifacts from a single piece of text (event body, prompt,
/// tool output — anything stringly-typed). Idempotent and free of I/O.
pub fn extract(text: &str) -> Artifacts {
    let mut a = Artifacts::default();

    // Commit hashes — 7 to 40 hex chars surrounded by word boundaries.
    // Word boundary on \b avoids matching inside longer non-hex tokens
    // (e.g. ULIDs are base32, but adjacent digits + letters could
    // technically pass — the boundary keeps matches clean).
    static_re(
        r"\b[0-9a-f]{7,40}\b",
        |m| {
            // Reject if all-digits (could be a year, an ID, a port).
            // A real abbreviated commit always has at least one letter.
            if m.chars().all(|c| c.is_ascii_digit()) {
                return;
            }
            a.commit_hashes.push(m.to_string());
        },
        text,
    );

    // GitHub / GitLab PR URLs.
    static_re(
        r"https?://[A-Za-z0-9.\-]+/[A-Za-z0-9_./\-]+/(?:pull|merge_requests)/\d+",
        |m| a.pr_urls.push(m.to_string()),
        text,
    );

    // Short PR references: "PR #51", "PR#51", "pull request #51". Anchored to
    // the PR / "pull request" keyword so a bare "#3" in prose (step #3, issue
    // #3) is NOT captured. Normalised to "PR #<n>" so it dedupes cleanly and
    // renders next to full URLs under the same `PRs:` group.
    if let Ok(re) = Regex::new(r"(?i)\b(?:PR|pull request)\s*#(\d+)\b") {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                a.pr_urls.push(format!("PR #{}", m.as_str()));
            }
        }
    }

    // Ticket IDs: ABC-123. At least 2 letters to avoid matching version
    // strings like v1-2 and minimum 1 digit.
    static_re(
        r"\b[A-Z]{2,}-\d+\b",
        |m| a.linked_issues.push(m.to_string()),
        text,
    );

    // File paths — heuristic: path-like tokens with at least one slash
    // (and an extension) OR a leading ./ . Path segments allow a
    // leading dot so `.docs/specs/auth.md`, `.github/workflows/ci.yml`
    // etc are captured as artifacts. Tight enough to skip prose, loose
    // enough to catch the common cases (src/foo.rs, ./bar.ts,
    // crates/tj-core/src/db.rs).
    static_re(
        r"(?:\./|\.?[A-Za-z0-9_\-]+/)+[A-Za-z0-9_.\-]+\.[A-Za-z0-9]{1,8}\b",
        |m| a.files.push(m.to_string()),
        text,
    );

    // Branch names from explicit git commands. v0.6.1: anchor the
    // pattern to `git ...` so that prose like "branches: commits, PRs,
    // files, branches names" does not capture the next word as a
    // branch. The bare-`branch <name>` form is intentionally dropped —
    // it caused too many false positives in journal events that
    // mention the word "branch" without naming one.
    if let Ok(re) =
        Regex::new(r"\bgit\s+(?:checkout\s+-b|switch\s+-c|branch)\s+([A-Za-z0-9._/\-]+)")
    {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                a.branch_names.push(m.as_str().to_string());
            }
        }
    }

    // Dedup in place — emit-time order matters for stable test output.
    dedup(&mut a.commit_hashes);
    dedup(&mut a.pr_urls);
    dedup(&mut a.linked_issues);
    dedup(&mut a.files);
    dedup(&mut a.branch_names);
    a
}

fn dedup(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|x| seen.insert(x.clone()));
}

fn static_re(pat: &str, mut f: impl FnMut(&str), text: &str) {
    if let Ok(re) = Regex::new(pat) {
        for m in re.find_iter(text) {
            f(m.as_str());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_commit_hash() {
        let a = extract("fixed in commit abc1234 and 9012abcdef");
        assert_eq!(a.commit_hashes, vec!["abc1234", "9012abcdef"]);
    }

    #[test]
    fn rejects_all_digit_commit_lookalikes() {
        // Year-like sequence, port numbers, etc.
        let a = extract("ran tests on port 12345 in 2026");
        assert!(a.commit_hashes.is_empty());
    }

    #[test]
    fn extracts_github_pr_url() {
        let a = extract("see https://github.com/Digital-Threads/Task-Journal/pull/42");
        assert_eq!(
            a.pr_urls,
            vec!["https://github.com/Digital-Threads/Task-Journal/pull/42"]
        );
    }

    #[test]
    fn extracts_linked_issues() {
        let a = extract("FIN-868 references JIRA-12345 and INC-7");
        assert_eq!(a.linked_issues, vec!["FIN-868", "JIRA-12345", "INC-7"]);
    }

    #[test]
    fn extracts_file_paths() {
        let a = extract("edited crates/tj-core/src/db.rs and ./README.md");
        assert!(a.files.contains(&"crates/tj-core/src/db.rs".to_string()));
        assert!(a.files.contains(&"./README.md".to_string()));
    }

    #[test]
    fn extracts_dot_prefixed_dirs() {
        // .docs/specs/*.md, .github/workflows/*.yml — leading-dot dirs
        // are spec/config holders we want surfaced as artifacts so the
        // pack ties decisions back to the document that justified them.
        let a = extract("see .docs/specs/auth.md and .github/workflows/ci.yml");
        assert!(a.files.contains(&".docs/specs/auth.md".to_string()));
        assert!(a.files.contains(&".github/workflows/ci.yml".to_string()));
    }

    #[test]
    fn extracts_branch_names() {
        // v0.6.1: only match explicit `git ...` commands so prose like
        // "branches: commits, PRs, names" no longer captures "names"
        // as a branch. Bare `switch -c` without `git ` prefix is also
        // ignored — keep the pattern conservative.
        let a = extract("git checkout -b FIN-868-fix-paygate-fee then git switch -c hotfix/abc");
        assert_eq!(
            a.branch_names,
            vec!["FIN-868-fix-paygate-fee", "hotfix/abc"]
        );
    }

    #[test]
    fn does_not_capture_branch_from_prose() {
        // Journal events mention `branches:` as a list header. The
        // pre-v0.6.1 extractor captured the next word ("names") as a
        // branch. The tightened regex requires explicit `git ` prefix.
        let a =
            extract("Artifacts groups: commits, PRs, issues, files, branches names listed below");
        assert!(
            a.branch_names.is_empty(),
            "regex must not pick up branches from prose, got: {:?}",
            a.branch_names
        );
    }

    #[test]
    fn merge_dedupes() {
        let mut a = Artifacts {
            commit_hashes: vec!["abc1234".into()],
            ..Default::default()
        };
        let b = Artifacts {
            commit_hashes: vec!["abc1234".into(), "def5678".into()],
            ..Default::default()
        };
        a.merge(b);
        assert_eq!(a.commit_hashes, vec!["abc1234", "def5678"]);
    }

    #[test]
    fn empty_text_yields_empty_artifacts() {
        let a = extract("");
        assert!(a.is_empty());
    }

    #[test]
    fn captures_short_pr_reference_but_not_bare_hash() {
        let a = extract("merged PR #51 and PR#52, see pull request #53");
        assert_eq!(a.pr_urls, vec!["PR #51", "PR #52", "PR #53"]);
        // bare hashes in prose must NOT be captured as PRs
        let b = extract("step #3 of 5, issue #7, line #42");
        assert!(b.pr_urls.is_empty(), "got: {:?}", b.pr_urls);
    }

    #[test]
    fn json_round_trip() {
        let a = Artifacts {
            commit_hashes: vec!["abc1234".into()],
            linked_issues: vec!["FIN-868".into()],
            ..Default::default()
        };
        let s = serde_json::to_string(&a).unwrap();
        let b: Artifacts = serde_json::from_str(&s).unwrap();
        assert_eq!(a, b);
    }
}
