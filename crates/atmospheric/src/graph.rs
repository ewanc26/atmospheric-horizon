//! Reading public Bluesky social graphs (`app.bsky.graph.getFollows`,
//! `getFollowers`, `getBlocks`, `getMutes`) — the client-side counterpart to
//! the join-gate list reads, all against the public AppView.
//!
//! Every read is unauthenticated and works logged out. A subject is a DID or
//! a handle; handles resolve through the same directory service OAuth login
//! uses. Each relation pages with `limit=100` until the cursor is
//! absent/blank, capped at [`MAX_PAGES`] pages. Profile views are reduced to
//! the DID, handle and display name the HUD needs.

use crate::error::{AtmosphericError, Result};

/// `app.bsky.graph.get*` page size (the lexicon caps at 100).
pub const PAGE_LIMIT: u32 = 100;

/// Defensive pagination cap (200 pages = 20 000 accounts at 100).
pub const MAX_PAGES: usize = 200;

/// Which social relation to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphKind {
    /// Accounts the subject follows (`app.bsky.graph.getFollows`).
    Follows,
    /// Accounts following the subject (`app.bsky.graph.getFollowers`).
    Followers,
    /// Accounts the subject blocks (`app.bsky.graph.getBlocks`).
    Blocks,
    /// Accounts the subject mutes (`app.bsky.graph.getMutes`).
    Mutes,
}

impl GraphKind {
    /// Parse the command word (`follows`, `followers`, `blocks`, `mutes`).
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "follows" => Some(Self::Follows),
            "followers" => Some(Self::Followers),
            "blocks" => Some(Self::Blocks),
            "mutes" => Some(Self::Mutes),
            _ => None,
        }
    }

    /// The command word, also used in events and status lines.
    pub fn key(self) -> &'static str {
        match self {
            Self::Follows => "follows",
            Self::Followers => "followers",
            Self::Blocks => "blocks",
            Self::Mutes => "mutes",
        }
    }

    /// The AppView endpoint that serves this relation.
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Follows => "app.bsky.graph.getFollows",
            Self::Followers => "app.bsky.graph.getFollowers",
            Self::Blocks => "app.bsky.graph.getBlocks",
            Self::Mutes => "app.bsky.graph.getMutes",
        }
    }

    /// The response field that carries the page's profile views.
    fn page_key(self) -> &'static str {
        match self {
            Self::Follows => "follows",
            Self::Followers => "followers",
            Self::Blocks => "blocks",
            Self::Mutes => "mutes",
        }
    }
}

/// One account in a social-graph read, reduced to what the HUD shows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GraphMember {
    pub did: String,
    pub handle: String,
    pub display_name: String,
}

impl GraphMember {
    /// Parse one AppView profile view; `None` when it has no DID.
    fn from_view(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            did: value["did"].as_str()?.to_string(),
            handle: value["handle"].as_str().unwrap_or_default().to_string(),
            display_name: value["displayName"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
    }
}

/// Resolve a graph subject (a DID or handle — not a record URI) to its DID.
pub async fn resolve_subject(
    http: &reqwest::Client,
    resolver: &crate::oauth::Resolver,
    raw: &str,
) -> Result<String> {
    let reference = crate::labels::parse_subject_reference(raw)?;
    let resolved = crate::labels::resolve_subject(http, resolver, &reference).await?;
    if resolved.collection.is_some() {
        return Err(AtmosphericError::Message(
            "want an account (a DID or a handle), not a record URI".into(),
        ));
    }
    Ok(resolved.pattern)
}

/// Read one relation of `actor` (a DID) from the AppView at `appview`,
/// paginating until the cursor ends. Unauthenticated.
pub async fn fetch_graph(
    http: &reqwest::Client,
    appview: &str,
    kind: GraphKind,
    actor: &str,
) -> Result<Vec<GraphMember>> {
    let base = appview.trim_end_matches('/');
    let mut members: Vec<GraphMember> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut request = http
            .get(format!("{base}/xrpc/{}", kind.endpoint()))
            .query(&[("actor", actor), ("limit", &PAGE_LIMIT.to_string())]);
        if let Some(cursor) = &cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        let response = request.send().await.map_err(AtmosphericError::protocol)?;
        let status = response.status();
        let body = response.text().await.map_err(AtmosphericError::protocol)?;
        if !status.is_success() {
            return Err(AtmosphericError::Message(format!(
                "{} for {actor} failed with HTTP {status}: {body}",
                kind.endpoint()
            )));
        }
        let page: serde_json::Value = serde_json::from_str(&body)?;
        members.extend(
            page[kind.page_key()]
                .as_array()
                .map(|views| views.iter().filter_map(GraphMember::from_view))
                .into_iter()
                .flatten(),
        );
        pages += 1;
        match page["cursor"].as_str() {
            Some(next) if !next.is_empty() && pages < MAX_PAGES => cursor = Some(next.to_string()),
            _ => break,
        }
    }
    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_kinds_round_trip_and_route_to_endpoints() {
        for (key, endpoint) in [
            ("follows", "app.bsky.graph.getFollows"),
            ("followers", "app.bsky.graph.getFollowers"),
            ("blocks", "app.bsky.graph.getBlocks"),
            ("mutes", "app.bsky.graph.getMutes"),
        ] {
            let kind = GraphKind::from_key(key).unwrap();
            assert_eq!(kind.key(), key);
            assert_eq!(kind.endpoint(), endpoint);
        }
        assert_eq!(GraphKind::from_key("likes"), None);
        assert_eq!(GraphKind::from_key(""), None);
    }

    #[test]
    fn fetch_graph_paginates_and_skips_did_less_views() {
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
                            r#"{"follows":[{"did":"did:plc:carol","handle":"carol.test","displayName":"Carol"}]}"#
                        } else {
                            r#"{"follows":[{"did":"did:plc:alice","handle":"alice.test","displayName":"Alice"},{"handle":"no-did.test"}],"cursor":"c2"}"#
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
            fetch_graph(&http, &url, GraphKind::Follows, "did:plc:actor")
                .await
                .unwrap()
        });
        assert_eq!(members.len(), 2, "the DID-less view is skipped");
        assert_eq!(members[0].handle, "alice.test");
        assert_eq!(members[0].display_name, "Alice");
        assert_eq!(members[1].did, "did:plc:carol");
        assert_eq!(members[1].display_name, "Carol");
        let hits = hits.lock().unwrap();
        assert_eq!(hits.len(), 2, "two pages fetched: {hits:?}");
        assert!(hits[0].starts_with("/xrpc/app.bsky.graph.getFollows?"));
        assert!(hits[0].contains("actor=did%3Aplc%3Aactor"));
        assert!(hits[0].contains("limit=100"));
        assert!(!hits[0].contains("cursor="));
        assert!(hits[1].contains("cursor=c2"));
    }

    #[test]
    fn fetch_graph_reports_http_errors() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                use std::io::{Read, Write};
                // Read the request first: closing with unread bytes can reset
                // the connection before the client sees the response.
                let mut chunk = [0u8; 4096];
                let _ = stream.read(&mut chunk);
                let payload = r#"{"error":"AccountNotFound","message":"no such actor"}"#;
                let response = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime.block_on(async {
            let http = reqwest::Client::new();
            fetch_graph(&http, &url, GraphKind::Blocks, "did:plc:missing")
                .await
                .unwrap_err()
        });
        assert!(error.to_string().contains("AccountNotFound"), "{error}");
    }
}
