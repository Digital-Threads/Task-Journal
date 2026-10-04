//! Resolve which session transcripts are in scope for a dream run.

use std::time::SystemTime;

/// A discovered session with its file modification time.
pub struct SessionFile {
    pub path: std::path::PathBuf,
    pub mtime: SystemTime,
}

/// Keep sessions modified strictly after `since` (the watermark as a
/// SystemTime). When `since` is None, all sessions are in scope.
/// `limit` (when Some) caps the result to the newest N.
pub fn in_scope(
    mut sessions: Vec<SessionFile>,
    since: Option<SystemTime>,
    limit: Option<usize>,
) -> Vec<std::path::PathBuf> {
    sessions.sort_by_key(|s| std::cmp::Reverse(s.mtime));
    let mut out: Vec<std::path::PathBuf> = sessions
        .into_iter()
        .filter(|s| match since {
            Some(t) => s.mtime > t,
            None => true,
        })
        .map(|s| s.path)
        .collect();
    if let Some(n) = limit {
        out.truncate(n);
    }
    out
}

/// Where an unscoped run may move the watermark, given every in-scope
/// session as `(mtime, mined_cleanly)`: the newest clean mtime older than
/// every failed or skipped session, so the next run (which keeps only
/// mtimes strictly after the watermark) still sees those. `None` = leave
/// the watermark where it is.
pub fn next_watermark(sessions: &[(SystemTime, bool)]) -> Option<SystemTime> {
    let first_bad = sessions
        .iter()
        .filter(|(_, clean)| !clean)
        .map(|(t, _)| *t)
        .min();

    sessions
        .iter()
        .filter(|(t, clean)| *clean && first_bad.is_none_or(|bad| *t < bad))
        .map(|(t, _)| *t)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn filters_by_since_and_caps_by_limit() {
        let s = vec![
            SessionFile {
                path: "a".into(),
                mtime: at(100),
            },
            SessionFile {
                path: "b".into(),
                mtime: at(200),
            },
            SessionFile {
                path: "c".into(),
                mtime: at(300),
            },
        ];
        // since = 150 → keeps b(200) and c(300), newest first
        let r = in_scope(s, Some(at(150)), None);
        assert_eq!(
            r,
            vec![std::path::PathBuf::from("c"), std::path::PathBuf::from("b")]
        );
    }

    #[test]
    fn watermark_advances_to_newest_when_all_clean() {
        let s = [(at(300), true), (at(100), true), (at(200), true)];
        assert_eq!(next_watermark(&s), Some(at(300)));
    }

    #[test]
    fn watermark_never_jumps_over_a_failed_session() {
        // 200 failed → stop at 100, so 200 and 300 are mined again.
        let s = [(at(100), true), (at(200), false), (at(300), true)];
        assert_eq!(next_watermark(&s), Some(at(100)));
        // Oldest failed → nothing to advance to.
        let s = [(at(100), false), (at(200), true)];
        assert_eq!(next_watermark(&s), None);
        // A clean session sharing the failed one's mtime can't be the
        // watermark either (`in_scope` keeps only mtime > watermark).
        let s = [(at(100), true), (at(200), true), (at(200), false)];
        assert_eq!(next_watermark(&s), Some(at(100)));
        assert_eq!(next_watermark(&[]), None);
    }

    #[test]
    fn none_since_keeps_all_limit_caps() {
        let s = vec![
            SessionFile {
                path: "a".into(),
                mtime: at(100),
            },
            SessionFile {
                path: "b".into(),
                mtime: at(200),
            },
            SessionFile {
                path: "c".into(),
                mtime: at(300),
            },
        ];
        let r = in_scope(s, None, Some(2));
        assert_eq!(
            r,
            vec![std::path::PathBuf::from("c"), std::path::PathBuf::from("b")]
        );
    }
}
