//! Reading public Bluesky lists (`app.bsky.graph.getList`) — the read side of
//! atpcraft's join gate (its `AtprotoListResolver`), reimplemented from the
//! lexicon semantics only: no code is copied from atpcraft (its repo carries
//! no licence).
//!
//! A list reference is either an AT-URI
//! `at://<did>/app.bsky.graph.list/<rkey>` or a bsky.app URL
//! `https://bsky.app/profile/<handle-or-did>/lists/<rkey>`. Members are
//! paged with `limit=100` until the cursor is absent/blank, capped at
//! [`MAX_PAGES`] pages; each member contributes `items[].subject.did`.

use crate::error::{AtmosphericError, Result};

/// The public AppView atpcraft queries for list reads (unauthenticated).
pub const DEFAULT_APPVIEW: &str = "https://public.api.bsky.app";

/// `app.bsky.graph.getList` page size (atpcraft uses 100).
pub const PAGE_LIMIT: u32 = 100;

/// Defensive pagination cap (atpcraft: 200 pages = 20 000 members at 100).
pub const MAX_PAGES: usize = 200;

/// `app.bsky.graph.getList` response (subset, as atpcraft models it).
#[derive(Debug, serde::Deserialize)]
struct GetListResponse {
    items: Vec<ListItemView>,
    cursor: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ListItemView {
    subject: ListSubject,
}

#[derive(Debug, serde::Deserialize)]
struct ListSubject {
    did: String,
}

/// A parsed list reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListRef {
    /// A canonical `at://<did>/app.bsky.graph.list/<rkey>` URI.
    AtUri(String),
    /// A bsky.app URL whose profile part still needs handle resolution:
    /// `https://bsky.app/profile/<handle>/lists/<rkey>`.
    NeedsResolve { handle: String, rkey: String },
}

/// Parse a list reference: an AT-URI or a bsky.app list URL. For bsky.app
/// URLs the owner must be a DID for a direct [`ListRef::AtUri`]; a handle
/// yields [`ListRef::NeedsResolve`] so the caller can resolve it.
pub fn parse_list_reference(raw: &str) -> Result<ListRef> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AtmosphericError::Message(
            "the list reference is empty".into(),
        ));
    }
    if trimmed.starts_with("at://") {
        if !trimmed.contains("/app.bsky.graph.list/") {
            return Err(AtmosphericError::Message(format!(
                "the list AT-URI must contain '/app.bsky.graph.list/' (got: {trimmed})"
            )));
        }
        return Ok(ListRef::AtUri(trimmed.to_string()));
    }
    // https://bsky.app/profile/<owner>/lists/<rkey>
    let Some(path) = trimmed
        .strip_prefix("https://bsky.app/profile/")
        .or_else(|| trimmed.strip_prefix("http://bsky.app/profile/"))
    else {
        return Err(AtmosphericError::Message(format!(
            "not a list reference (want an at://…app.bsky.graph.list/… URI or a bsky.app list URL): {trimmed}"
        )));
    };
    let mut parts = path.split('/');
    let Some(owner) = parts.next().filter(|s| !s.is_empty()) else {
        return Err(AtmosphericError::Message(
            "the list URL has no profile".into(),
        ));
    };
    if parts.next() != Some("lists") {
        return Err(AtmosphericError::Message(format!(
            "the bsky.app URL must be /profile/<owner>/lists/<rkey> (got: {trimmed})"
        )));
    }
    let Some(rkey) = parts.next().filter(|s| !s.is_empty()) else {
        return Err(AtmosphericError::Message("the list URL has no rkey".into()));
    };
    if owner.starts_with("did:") {
        Ok(ListRef::AtUri(format!(
            "at://{owner}/app.bsky.graph.list/{rkey}"
        )))
    } else {
        Ok(ListRef::NeedsResolve {
            handle: owner.to_string(),
            rkey: rkey.to_string(),
        })
    }
}

/// Resolve a list reference to its canonical AT-URI, resolving a bsky.app
/// URL's handle owner via the same directory service OAuth login uses.
pub async fn resolve_list_reference(
    http: &reqwest::Client,
    resolver: &crate::oauth::Resolver,
    raw: &str,
) -> Result<String> {
    match parse_list_reference(raw)? {
        ListRef::AtUri(uri) => Ok(uri),
        ListRef::NeedsResolve { handle, rkey } => {
            let did = crate::oauth::resolve_handle(http, resolver, &handle).await?;
            Ok(format!("at://{did}/app.bsky.graph.list/{rkey}"))
        }
    }
}

/// Fetch all member DIDs of the list at `list` (an AT-URI) from the AppView at
/// `appview`, paginating until the cursor ends. Unauthenticated — the public
/// AppView serves this.
pub async fn fetch_list_members(
    http: &reqwest::Client,
    appview: &str,
    list: &str,
) -> Result<Vec<String>> {
    let base = appview.trim_end_matches('/');
    let mut members: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut request = http
            .get(format!("{base}/xrpc/app.bsky.graph.getList"))
            .query(&[("list", list), ("limit", &PAGE_LIMIT.to_string())]);
        if let Some(cursor) = &cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        let response = request.send().await.map_err(AtmosphericError::protocol)?;
        let status = response.status();
        let body = response.text().await.map_err(AtmosphericError::protocol)?;
        if !status.is_success() {
            return Err(AtmosphericError::Message(format!(
                "getList for {list} failed with HTTP {status}: {body}"
            )));
        }
        let page: GetListResponse = serde_json::from_str(&body)?;
        members.extend(page.items.into_iter().map(|item| item.subject.did));
        pages += 1;
        match page.cursor {
            Some(next) if !next.is_empty() && pages < MAX_PAGES => cursor = Some(next),
            _ => break,
        }
    }
    Ok(members)
}

/// The outcome of a join-gate check (M15): the resolved list AT-URI, the
/// subject's DID, and whether the DID is on the list. Fail-closed like
/// atpcraft's gate: any resolution or list error propagates to the caller,
/// which reports it as a deny-with-reason — never a silent allow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateOutcome {
    pub list: String,
    pub subject: String,
    pub allowed: bool,
}

/// Check whether `subject` (a DID or handle — not a record URI) is on the
/// list referenced by `list_reference` (M15, atpcraft's
/// `AtprotoListResolver.isMember` decision): resolve both, page the members
/// from the AppView, and compare DIDs.
pub async fn check_gate(
    http: &reqwest::Client,
    resolver: &crate::oauth::Resolver,
    appview: &str,
    list_reference: &str,
    subject: &str,
) -> Result<GateOutcome> {
    let list = resolve_list_reference(http, resolver, list_reference).await?;
    let reference = crate::labels::parse_subject_reference(subject)?;
    let resolved = crate::labels::resolve_subject(http, resolver, &reference).await?;
    if resolved.collection.is_some() {
        return Err(AtmosphericError::Message(
            "want an account (a DID or a handle), not a record URI".into(),
        ));
    }
    let did = resolved.pattern; // the bare DID for accounts
    let members = fetch_list_members(http, appview, &list).await?;
    let allowed = members.iter().any(|member| member == &did);
    Ok(GateOutcome {
        list,
        subject: did,
        allowed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_at_uris_and_bsky_urls() {
        assert_eq!(
            parse_list_reference("at://did:plc:alice/app.bsky.graph.list/3abc").unwrap(),
            ListRef::AtUri("at://did:plc:alice/app.bsky.graph.list/3abc".into())
        );
        assert_eq!(
            parse_list_reference("https://bsky.app/profile/did:plc:alice/lists/3abc").unwrap(),
            ListRef::AtUri("at://did:plc:alice/app.bsky.graph.list/3abc".into())
        );
        assert_eq!(
            parse_list_reference("https://bsky.app/profile/alice.test/lists/3abc").unwrap(),
            ListRef::NeedsResolve {
                handle: "alice.test".into(),
                rkey: "3abc".into()
            }
        );
        assert!(parse_list_reference("at://did:plc:alice/app.bsky.feed.post/3abc").is_err());
        assert!(parse_list_reference("https://example.com/x").is_err());
        assert!(parse_list_reference("   ").is_err());
    }

    #[test]
    fn fetch_paginates_until_the_cursor_ends() {
        // A two-page stub AppView.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let hits = std::sync::Arc::clone(&hits);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let hits = std::sync::Arc::clone(&hits);
                    std::thread::spawn(move || {
                        use std::io::{Read, Write};
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 4096];
                        let _ = stream.read(&mut chunk);
                        buf.extend_from_slice(&chunk);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                        hits.lock().unwrap().push(path.clone());
                        let payload = if path.contains("cursor=c2") {
                            r#"{"items":[{"subject":{"did":"did:plc:carol"}}]}"#
                        } else {
                            r#"{"items":[{"subject":{"did":"did:plc:alice"}},{"subject":{"did":"did:plc:bob"}}],"cursor":"c2"}"#
                        };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    });
                }
            });
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let members = runtime.block_on(async {
            let http = reqwest::Client::new();
            fetch_list_members(&http, &url, "at://did:plc:owner/app.bsky.graph.list/3abc")
                .await
                .unwrap()
        });
        assert_eq!(
            members,
            vec![
                "did:plc:alice".to_string(),
                "did:plc:bob".to_string(),
                "did:plc:carol".to_string()
            ]
        );
        let hits = hits.lock().unwrap();
        assert_eq!(hits.len(), 2, "two pages were fetched: {hits:?}");
        assert!(hits[0].contains("limit=100"));
        assert!(!hits[0].contains("cursor="));
        assert!(hits[1].contains("cursor=c2"));
    }

    #[test]
    fn check_gate_resolves_both_sides_and_decides_membership() {
        // The stub doubles as the handle service and the AppView.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                std::thread::spawn(move || {
                    use std::io::{Read, Write};
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let _ = stream.read(&mut chunk);
                    buf.extend_from_slice(&chunk);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                    let payload = if path.starts_with("/xrpc/com.atproto.identity.resolveHandle") {
                        let handle = path.split("handle=").nth(1).unwrap_or("").to_string();
                        let did: String = match handle.as_str() {
                            "owner.test" => "did:plc:owner".into(),
                            "alice.test" => "did:plc:alice".into(),
                            other => format!("did:plc:{}", other.trim_end_matches(".test")),
                        };
                        format!(r#"{{"did":"{did}"}}"#)
                    } else if path.starts_with("/xrpc/app.bsky.graph.getList") {
                        r#"{"items":[{"subject":{"did":"did:plc:alice"}},{"subject":{"did":"did:plc:bob"}}]}"#.to_string()
                    } else {
                        r#"{"error":"UnknownEndpoint"}"#.to_string()
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });
        let resolver = crate::oauth::Resolver {
            handle_service: url.clone(),
            plc_directory: url.clone(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let http = reqwest::Client::new();
            // A bsky.app URL with a handle owner, and a handle subject: both
            // resolve through the stub; alice is on the list.
            let outcome = check_gate(
                &http,
                &resolver,
                &url,
                "https://bsky.app/profile/owner.test/lists/3abc",
                "alice.test",
            )
            .await
            .unwrap();
            assert_eq!(
                outcome,
                GateOutcome {
                    list: "at://did:plc:owner/app.bsky.graph.list/3abc".into(),
                    subject: "did:plc:alice".into(),
                    allowed: true,
                }
            );
            // mallory is not on the list.
            let outcome = check_gate(
                &http,
                &resolver,
                &url,
                "at://did:plc:owner/app.bsky.graph.list/3abc",
                "did:plc:mallory",
            )
            .await
            .unwrap();
            assert!(!outcome.allowed);
            assert_eq!(outcome.subject, "did:plc:mallory");
            // A record URI is not an account: error (fail closed).
            let error = check_gate(
                &http,
                &resolver,
                &url,
                "at://did:plc:owner/app.bsky.graph.list/3abc",
                "at://did:plc:alice/app.bsky.feed.post/3abc",
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("want an account"), "{error}");
            // A bad list reference errors before any list fetch.
            assert!(
                check_gate(
                    &http,
                    &resolver,
                    &url,
                    "https://example.com/nope",
                    "did:plc:alice"
                )
                .await
                .is_err()
            );
        });
    }
}
