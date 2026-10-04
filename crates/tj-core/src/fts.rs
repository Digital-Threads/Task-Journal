//! FTS5 query sanitization.
//!
//! SQLite FTS5 treats almost any punctuation (`-`, `:`, `*`, `.`, `,`, `'`,
//! `+`, `^`, `{`, `?`, …) and the bare words `AND`/`OR`/`NOT`/`NEAR` as
//! syntax. A raw query like `OPS-306` parses as column-prefix `OPS-`
//! followed by reference `306`, raising `no such column: 306` at runtime.
//!
//! [`sanitize_query`] splits the query on whitespace and wraps every token
//! in double quotes, so each one is an FTS5 string matched as the unicode61
//! tokenizer splits it, and the tokens are implicitly AND-ed. Internal `"`
//! characters are doubled per FTS5 escape rules.

/// Quote every whitespace-separated token as its own FTS5 string. Any
/// punctuation inside a token is then literal, operator words lose their
/// meaning, and a multi-token query keeps its default AND semantics.
pub fn sanitize_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build a `LIKE` pattern equivalent to a free-text search — used as a
/// last-resort fallback when an FTS5 search returns no hits and the
/// caller wants to match the raw substring against `search_fts.text`.
/// SQL `LIKE` escapes are not applied; callers MUST pass the result as
/// a bound parameter, not interpolate it.
pub fn like_pattern(query: &str) -> String {
    format!("%{query}%")
}

#[cfg(test)]
mod tests {
    use super::{like_pattern, sanitize_query};

    /// Count rows of a throwaway FTS5 table matching `sanitize_query(query)`.
    /// `Err` means FTS5 rejected the sanitized query as syntax.
    fn fts_hits(rows: &[&str], query: &str) -> rusqlite::Result<i64> {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(text);")
            .unwrap();
        for row in rows {
            conn.execute("INSERT INTO t(text) VALUES (?1)", [row])
                .unwrap();
        }

        conn.query_row(
            "SELECT COUNT(*) FROM t WHERE t MATCH ?1",
            [sanitize_query(query)],
            |r| r.get(0),
        )
    }

    #[test]
    fn plain_word_is_quoted_as_one_token() {
        assert_eq!(sanitize_query("hello"), "\"hello\"");
    }

    #[test]
    fn multi_word_quotes_each_token_for_default_and_search() {
        assert_eq!(sanitize_query("bulk repack"), "\"bulk\" \"repack\"");
    }

    #[test]
    fn cyrillic_tokens_are_quoted() {
        assert_eq!(sanitize_query("слим модели"), "\"слим\" \"модели\"");
    }

    #[test]
    fn punctuation_and_operator_words_never_raise_fts5_syntax_errors() {
        let rows = ["release v1.2, don't c++ AND or NOT near"];
        for q in [
            "v1.2",
            "a, b",
            "don't",
            "c++",
            "^start",
            "{x}",
            "why?",
            "AND",
            "OR",
            "NOT",
            "NEAR",
            "this AND",
            "NEAR(a b)",
            "a + b",
        ] {
            assert!(
                fts_hits(&rows, q).is_ok(),
                "FTS5 rejected sanitized query {q:?}: {:?}",
                fts_hits(&rows, q)
            );
        }
    }

    #[test]
    fn multi_word_query_with_hyphen_is_and_of_tokens_not_a_phrase() {
        let rows = ["OPS-306 needs a quick fix"];
        assert_eq!(fts_hits(&rows, "fix OPS-306").unwrap(), 1);
        assert_eq!(fts_hits(&rows, "fix OPS-307").unwrap(), 0);
    }

    #[test]
    fn hyphenated_id_gets_phrase_quoted() {
        assert_eq!(sanitize_query("OPS-306"), "\"OPS-306\"");
    }

    #[test]
    fn slash_path_gets_phrase_quoted() {
        assert_eq!(sanitize_query("src/main.rs"), "\"src/main.rs\"");
    }

    #[test]
    fn colon_gets_phrase_quoted() {
        assert_eq!(sanitize_query("ttl:30s"), "\"ttl:30s\"");
    }

    #[test]
    fn star_gets_phrase_quoted() {
        assert_eq!(sanitize_query("foo*bar"), "\"foo*bar\"");
    }

    #[test]
    fn parens_get_phrase_quoted() {
        assert_eq!(sanitize_query("func()"), "\"func()\"");
    }

    #[test]
    fn embedded_quote_is_doubled() {
        assert_eq!(sanitize_query("say \"hi\""), "\"say\" \"\"\"hi\"\"\"");
    }

    #[test]
    fn empty_query_stays_empty() {
        assert_eq!(sanitize_query(""), "");
        assert_eq!(sanitize_query("   "), "");
    }

    #[test]
    fn like_pattern_wraps_with_percent_signs() {
        assert_eq!(like_pattern("OPS-306"), "%OPS-306%");
    }
}
