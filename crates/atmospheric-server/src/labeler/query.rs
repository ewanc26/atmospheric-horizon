//! Parsing for `com.atproto.label.queryLabels` query parameters.
//!
//! Array parameters accept both repeated keys and the comma-separated form
//! atpcraft uses. Unknown parameters are ignored, and an invalid `limit` or
//! `cursor` falls back to the service defaults rather than failing the query.

/// A parsed `queryLabels` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub patterns: Vec<String>,
    pub sources: Vec<String>,
    pub limit: usize,
    pub cursor: u64,
}

impl Query {
    /// Parse the optional `cursor` of `subscribeLabels`. A missing, negative or
    /// non-numeric cursor means the full history, matching `queryLabels` and
    /// atpcraft's lenient parameter handling.
    pub fn subscribe_cursor(raw: &str) -> Option<u64> {
        form_urlencoded::parse(raw.as_bytes()).find_map(|(key, value)| {
            (key == "cursor")
                .then(|| value.parse::<u64>().ok())
                .flatten()
        })
    }

    /// Parse a raw query string. `uriPatterns` is required and at least one
    /// non-empty pattern must remain after splitting.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut patterns = Vec::new();
        let mut sources = Vec::new();
        let mut limit = None;
        let mut cursor = None;
        for (key, value) in form_urlencoded::parse(raw.as_bytes()) {
            match key.as_ref() {
                "uriPatterns" => patterns.extend(split_list(&value)),
                "sources" => sources.extend(split_list(&value)),
                "limit" => {
                    limit = value
                        .parse::<i64>()
                        .ok()
                        .map(|value| value.clamp(1, 250) as usize);
                }
                "cursor" => cursor = value.parse::<u64>().ok(),
                _ => {}
            }
        }
        if patterns.is_empty() {
            return Err("Parameter 'uriPatterns' is required".into());
        }
        Ok(Self {
            patterns,
            sources,
            limit: limit.unwrap_or(50),
            cursor: cursor.unwrap_or(0),
        })
    }
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_and_comma_separated_arrays_are_accepted() {
        let query = Query::parse(
            "uriPatterns=did:plc%3Aalice&uriPatterns=at%3A%2F%2Fx%2Fapp.bsky.feed.post%2F1,did:plc%3Abob&sources=did:plc%3Alabeler",
        )
        .unwrap();
        assert_eq!(
            query.patterns,
            [
                "did:plc:alice",
                "at://x/app.bsky.feed.post/1",
                "did:plc:bob"
            ]
        );
        assert_eq!(query.sources, ["did:plc:labeler"]);
        assert_eq!(query.limit, 50);
        assert_eq!(query.cursor, 0);
    }

    #[test]
    fn missing_patterns_and_bad_numbers_are_handled() {
        assert!(Query::parse("").is_err());
        assert!(Query::parse("uriPatterns=").is_err());

        let query = Query::parse("uriPatterns=*%26x&limit=0&cursor=nope").unwrap();
        assert_eq!(query.patterns, ["*&x"]);
        assert_eq!(query.limit, 1);
        assert_eq!(query.cursor, 0);

        let query = Query::parse("uriPatterns=*&limit=999").unwrap();
        assert_eq!(query.limit, 250);
        assert_eq!(Query::parse("uriPatterns=*&cursor=12").unwrap().cursor, 12);
    }

    #[test]
    fn subscription_cursor_is_optional_and_lenient() {
        assert_eq!(Query::subscribe_cursor("cursor=0"), Some(0));
        assert_eq!(Query::subscribe_cursor("cursor=42&other=x"), Some(42));
        assert_eq!(Query::subscribe_cursor("other=x"), None);
        assert_eq!(Query::subscribe_cursor("cursor=-1"), None);
        assert_eq!(Query::subscribe_cursor("cursor=nope"), None);
    }
}
