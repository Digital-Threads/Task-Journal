//! Human-readable title derivation for auto-opened tasks.
//!
//! When the journal auto-opens a task it has only a raw chat chunk to name it
//! with. Early versions took the first non-empty line verbatim — which, at
//! session start, is often terminal scrollback (`685] INFO: Mapped {…}`), a
//! shell prompt (`user@host:~$ …`), or the journal's own resume banner
//! (`[Task Journal resumed: …]`). Those leak into the task list and the
//! Claude Code session name.
//!
//! `humanize_title` scans for the first line that looks like natural-language
//! intent and returns it cleaned + truncated. When nothing qualifies it
//! returns `None` so the caller declines to auto-open rather than label a task
//! with machine noise.

/// Pick the first natural-language line from `raw` as a task title.
/// Returns `None` when the input is only logs / banners / shell prompts / JSON.
pub fn humanize_title(raw: &str) -> Option<String> {
    intent_line(raw).map(|l| truncate(l, 80))
}

/// Like [`humanize_title`] but truncated to `max` chars — used for the task
/// goal, which tolerates a longer sentence than the 80-char title.
pub fn humanize_goal(raw: &str, max: usize) -> Option<String> {
    intent_line(raw).map(|l| truncate(l, max))
}

/// First line of `raw` that reads like a human wrote it on purpose.
fn intent_line(raw: &str) -> Option<&str> {
    raw.lines().map(str::trim).find(|l| is_human_intent(l))
}

fn is_human_intent(line: &str) -> bool {
    let l = line.trim();
    if l.chars().count() < 6 {
        return false;
    }
    // Slash command, shell line, markdown heading, JSON/array/tag, log timestamp.
    if let Some(c) = l.chars().next() {
        if matches!(c, '/' | '$' | '#' | '{' | '[' | '<' | '|' | '`') {
            return false;
        }
    }
    if l.starts_with("http://") || l.starts_with("https://") {
        return false;
    }
    if l.contains("Task Journal resumed") {
        return false;
    }
    if looks_like_log(l) || looks_like_shell_prompt(l) {
        return false;
    }
    // Real intent: has letters and at least two whitespace-separated words.
    l.chars().any(char::is_alphabetic) && l.split_whitespace().count() >= 2
}

/// `685] INFO: Mapped …`, `… INFO: …`, `… ERROR: …` etc.
fn looks_like_log(l: &str) -> bool {
    // "<digits>] " near the start (a numeric log index).
    if let Some(rb) = l.find(']') {
        if rb > 0 && rb <= 8 && l[..rb].chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    const LEVELS: [&str; 5] = ["INFO:", "WARN:", "ERROR:", "DEBUG:", "TRACE:"];
    LEVELS.iter().any(|lvl| l.contains(lvl))
}

/// `shahinyanm@DESKTOP-KM9V32O:~/docker-local-env$ claude …`
fn looks_like_shell_prompt(l: &str) -> bool {
    let Some(at) = l.find('@') else {
        return false;
    };
    if at >= 40 {
        return false;
    }
    let after = &l[at + 1..];
    after.contains(":~") || after.contains(":/") || after.contains("$ ") || after.contains("# ")
}

/// Char-safe truncate with an ellipsis when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_numeric_log_line() {
        assert_eq!(
            humanize_title("685] INFO: Mapped {/rest-api/paymentlnk-notify, POST} route"),
            None
        );
    }

    #[test]
    fn rejects_timestamped_log_line() {
        assert_eq!(
            humanize_title("[11:13:30.685] INFO: Mapped {/rest-api/qiwi-notify, POST}"),
            None
        );
    }

    #[test]
    fn rejects_resume_banner() {
        assert_eq!(
            humanize_title("[Task Journal resumed: tj-ma8393mg0d — Так, давай ты сделаешь match]"),
            None
        );
    }

    #[test]
    fn rejects_shell_prompt() {
        assert_eq!(
            humanize_title(
                "shahinyanm@DESKTOP-KM9V32O:~/docker-local-env$ claude plugin marketplace update"
            ),
            None
        );
    }

    #[test]
    fn rejects_json_and_paths() {
        assert_eq!(humanize_title("{\"command\":\"task-journal\"}"), None);
        assert_eq!(humanize_title("/home/shahinyanm/www/claude-memory"), None);
    }

    #[test]
    fn accepts_plain_prose() {
        assert_eq!(
            humanize_title("Fix the auth bug in the payment middleware"),
            Some("Fix the auth bug in the payment middleware".to_string())
        );
    }

    #[test]
    fn accepts_russian_prose() {
        assert_eq!(
            humanize_title("Сделай так чтобы имя сессии не ломалось"),
            Some("Сделай так чтобы имя сессии не ломалось".to_string())
        );
    }

    #[test]
    fn picks_first_human_line_after_noise() {
        let raw = "685] INFO: Mapped {/x, POST}\n\
                   shahinyanm@host:~$ ls\n\
                   Add validation for negative order amounts";
        assert_eq!(
            humanize_title(raw),
            Some("Add validation for negative order amounts".to_string())
        );
    }

    #[test]
    fn truncates_long_title_to_80_chars() {
        let long = "Implement ".repeat(20);
        let out = humanize_title(&long).unwrap();
        assert!(
            out.chars().count() <= 80,
            "got {} chars",
            out.chars().count()
        );
        assert!(out.ends_with('…'));
    }

    #[test]
    fn none_when_only_noise() {
        let raw = "685] INFO: a\n$ git status\n/clear\n{\"k\":1}";
        assert_eq!(humanize_title(raw), None);
    }

    #[test]
    fn goal_allows_longer_text() {
        let line = "Make sure every coding session always records its reasoning chain \
                    into the task journal without spawning a model";
        let goal = humanize_goal(line, 200).unwrap();
        assert!(goal.chars().count() > 80);
        assert!(goal.starts_with("Make sure every coding session"));
    }
}
