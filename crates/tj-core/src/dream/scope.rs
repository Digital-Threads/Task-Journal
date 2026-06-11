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
            vec![
                std::path::PathBuf::from("c"),
                std::path::PathBuf::from("b")
            ]
        );
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
            vec![
                std::path::PathBuf::from("c"),
                std::path::PathBuf::from("b")
            ]
        );
    }
}
