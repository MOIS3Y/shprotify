//! Literal phrase-prefix queries and typed catalog search summaries.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Entity categories supported by the shared catalog search.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, sqlx::Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub(crate) enum SearchKind {
    /// Artist display names.
    Artist,
    /// Release titles.
    Release,
    /// Recording titles, independent of file availability.
    Recording,
}

/// One catalog identity; clients fetch details from its existing API resource.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct SearchHit {
    /// Category identifying the corresponding catalog endpoint.
    pub(crate) kind: SearchKind,
    /// Stable entity identifier; scoped to its category.
    pub(crate) id: i64,
    /// Artist name or release/recording title, preserving catalog spelling.
    pub(crate) title: String,
}

/// Validate text and quote it as a literal FTS phrase with a final prefix.
///
/// Tokens must be adjacent and ordered; only the last token may be incomplete.
/// Quotes/operators are never interpreted as user-supplied FTS syntax.
pub(crate) fn phrase(text: &str) -> Result<String, &'static str> {
    let text = text.trim();
    if text.is_empty()
        || text.chars().count() > 255
        || text.chars().any(char::is_control)
    {
        return Err("q must contain 1–255 characters without controls");
    }
    Ok(format!("\"{}\"*", text.replace('"', "\"\"")))
}

#[cfg(test)]
mod tests {
    //! Query bounds and escaping independent of SQLite storage.

    use super::*;

    /// Unicode characters are counted individually and syntax stays literal.
    #[test]
    fn validates_bounds_and_quotes_fts_syntax() {
        assert_eq!(phrase(" Кино ").unwrap(), "\"Кино\"*");
        assert_eq!(phrase("a\" OR b").unwrap(), "\"a\"\" OR b\"*");
        assert!(phrase(&"я".repeat(255)).is_ok());
        for invalid in [
            "".into(),
            " ".into(),
            "a\nb".into(),
            "a\0b".into(),
            "я".repeat(256),
        ] {
            assert!(phrase(&invalid).is_err());
        }
    }
}
