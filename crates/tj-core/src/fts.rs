//! FTS5 query sanitization.
//!
//! SQLite FTS5 treats `-`, `:`, `*`, `(`, `)`, `"`, and `/` as syntax.
//! A raw query like `OPS-306` parses as column-prefix `OPS-` followed by
//! reference `306`, raising `no such column: 306` at runtime.
//!
//! [`sanitize_query`] wraps queries containing any metacharacter in
//! double quotes, turning them into FTS5 phrase queries that match the
//! sequence of tokens as the unicode61 tokenizer split them. Internal
//! `"` characters are doubled per FTS5 escape rules.

/// Wrap a query in FTS5 phrase quotes if it contains any character the
/// FTS5 parser treats as syntax. Strings that only contain word
/// characters / whitespace pass through untouched so multi-term
/// queries keep their default AND semantics.
pub fn sanitize_query(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }
    let needs_quote = query
        .chars()
        .any(|c| matches!(c, '-' | '"' | '*' | ':' | '(' | ')' | '/'));
    if !needs_quote {
        return query.to_string();
    }
    let escaped = query.replace('"', "\"\"");
    format!("\"{escaped}\"")
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

    #[test]
    fn plain_word_passes_through() {
        assert_eq!(sanitize_query("hello"), "hello");
    }

    #[test]
    fn multi_word_passes_through_for_default_and_search() {
        assert_eq!(sanitize_query("bulk repack"), "bulk repack");
    }

    #[test]
    fn cyrillic_passes_through() {
        assert_eq!(sanitize_query("слим модели"), "слим модели");
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
        assert_eq!(sanitize_query("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn empty_query_stays_empty() {
        assert_eq!(sanitize_query(""), "");
    }

    #[test]
    fn like_pattern_wraps_with_percent_signs() {
        assert_eq!(like_pattern("OPS-306"), "%OPS-306%");
    }
}
