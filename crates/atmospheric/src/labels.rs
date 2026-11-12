//! Reading labels (`com.atproto.label.*`) — the client-side read half of the
//! labeler roadmap item (#29): (a) labels on a subject via the AppView view
//! fields (`labels` on actor/post/list views), and (b) resolving a labeler's
//! `#atproto_labeler` DID-document service endpoint and querying its
//! `com.atproto.label.queryLabels`. Label *issuance* stays server-side.
//!
//! Wire shapes come from the lexicons/spec only: a label is a
//! `com.atproto.label.defs#label` (`{ver, src, uri, cid?, val, neg?, cts,
//! exp?, sig?}`); account labels carry the bare DID as `uri`, record labels
//! the full `at://` URI. No code is copied from atpcraft (its repo carries no
//! licence) — atpcraft's labeler answers the same spec endpoints.
//!
//! Both reads are unauthenticated: the public AppView serves the view
//! endpoints, and labelers serve `queryLabels` publicly.

use crate::error::{AtmosphericError, Result};

/// `com.atproto.label.queryLabels` page size (the lexicon caps at 250).
pub const QUERY_LIMIT: u32 = 100;

/// Defensive pagination cap for `queryLabels` (100 pages = 10 000 labels).
pub const MAX_PAGES: usize = 100;

/// One label, reduced to what the HUD shows (`com.atproto.label.defs#label`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabelDigest {
    /// The label value (`val`), e.g. "online" or "spam".
    pub val: String,
    /// The labeler's DID (`src`).
    pub src: String,
    /// A negation label (overwrites a previous label of the same value).
    #[serde(default)]
    pub neg: bool,
    /// Creation timestamp (`cts`).
    pub cts: String,
    /// Expiry timestamp (`exp`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<String>,
}

impl LabelDigest {
    /// Parse one `com.atproto.label.defs#label` object; `None` when malformed.
    fn from_json(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            val: value["val"].as_str()?.to_string(),
            src: value["src"].as_str()?.to_string(),
            neg: value["neg"].as_bool().unwrap_or(false),
            cts: value["cts"].as_str()?.to_string(),
            exp: value["exp"].as_str().map(str::to_owned),
        })
    }
}

/// A parsed label subject: what `/atmospheric labels <subject>` accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectRef {
    /// A full AT-URI: `at://<did>` (an account) or
    /// `at://<did>/<collection>/<rkey>` (a record).
    AtUri(String),
    /// A bare DID (an account).
    Did(String),
    /// A handle that still needs resolving; `collection`/`rkey` are set when
    /// the reference was a bsky.app record URL.
    NeedsResolve {
        handle: String,
        collection: Option<String>,
        rkey: Option<String>,
    },
}

/// A subject with any handle resolved to a DID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSubject {
    /// The canonical AT-URI: `at://<did>` for accounts, the full URI for
    /// records.
    pub at_uri: String,
    /// The label-subject pattern for `queryLabels`: the bare DID for accounts
    /// (account labels carry the DID as `uri`), the full AT-URI for records.
    pub pattern: String,
    /// The record collection, `None` for accounts.
    pub collection: Option<String>,
}

/// Split `at://<authority>[/<collection>/<rkey>]` into its parts.
fn split_at_uri(uri: &str) -> Option<(String, Option<String>, Option<String>)> {
    let rest = uri.strip_prefix("at://")?;
    let mut parts = rest.splitn(3, '/');
    let authority = parts.next().filter(|s| !s.is_empty())?.to_string();
    let collection = parts.next().map(str::to_owned);
    let rkey = parts.next().map(str::to_owned);
    match (&collection, &rkey) {
        (Some(_), None) => None, // a collection without an rkey is not a record
        _ => Some((authority, collection, rkey)),
    }
}

/// Parse a label subject: an AT-URI, a DID, a handle, or a bsky.app profile /
/// post / list URL.
pub fn parse_subject_reference(raw: &str) -> Result<SubjectRef> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AtmosphericError::Message(
            "the subject reference is empty".into(),
        ));
    }
    if trimmed.starts_with("at://") {
        if split_at_uri(trimmed).is_none() {
            return Err(AtmosphericError::Message(format!(
                "malformed AT-URI (want at://<did>[/<collection>/<rkey>]): {trimmed}"
            )));
        }
        return Ok(SubjectRef::AtUri(trimmed.to_string()));
    }
    if trimmed.starts_with("did:") {
        return Ok(SubjectRef::Did(trimmed.to_string()));
    }
    // https://bsky.app/profile/<owner>[/post/<rkey> | /lists/<rkey>]
    if let Some(path) = trimmed
        .strip_prefix("https://bsky.app/profile/")
        .or_else(|| trimmed.strip_prefix("http://bsky.app/profile/"))
    {
        let mut parts = path.split('/');
        let Some(owner) = parts.next().filter(|s| !s.is_empty()) else {
            return Err(AtmosphericError::Message(
                "the bsky.app URL has no profile".into(),
            ));
        };
        let tail: Vec<&str> = parts.collect();
        let (collection, rkey) = match tail.as_slice() {
            [] => (None, None),
            ["post", rkey] if !rkey.is_empty() => (
                Some("app.bsky.feed.post".to_string()),
                Some((*rkey).to_string()),
            ),
            ["lists", rkey] if !rkey.is_empty() => (
                Some("app.bsky.graph.list".to_string()),
                Some((*rkey).to_string()),
            ),
            _ => {
                return Err(AtmosphericError::Message(format!(
                    "not a subject reference (want an at:// URI, a DID, a handle, or a bsky.app profile/post/list URL): {trimmed}"
                )));
            }
        };
        if owner.starts_with("did:") {
            let at_uri = match (&collection, &rkey) {
                (Some(collection), Some(rkey)) => format!("at://{owner}/{collection}/{rkey}"),
                _ => format!("at://{owner}"),
            };
            return Ok(SubjectRef::AtUri(at_uri));
        }
        return Ok(SubjectRef::NeedsResolve {
            handle: owner.to_string(),
            collection,
            rkey,
        });
    }
    // A bare handle (atproto handles: lowercase ascii letters, digits, '-',
    // '.', at least one dot).
    if !trimmed.contains(char::is_whitespace)
        && trimmed.contains('.')
        && trimmed
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
    {
        return Ok(SubjectRef::NeedsResolve {
            handle: trimmed.to_string(),
            collection: None,
            rkey: None,
        });
    }
    Err(AtmosphericError::Message(format!(
        "not a subject reference (want an at:// URI, a DID, a handle, or a bsky.app profile/post/list URL): {trimmed}"
    )))
}

/// Resolve a parsed subject to DIDs (handles go through the same
/// `resolveHandle` service OAuth login uses).
pub async fn resolve_subject(
    http: &reqwest::Client,
    resolver: &crate::oauth::Resolver,
    reference: &SubjectRef,
) -> Result<ResolvedSubject> {
    let (authority, collection, rkey) = match reference {
        SubjectRef::AtUri(uri) => split_at_uri(uri).expect("validated by the parser"),
        SubjectRef::Did(did) => (did.clone(), None, None),
        SubjectRef::NeedsResolve {
            handle,
            collection,
            rkey,
        } => {
            let did = crate::oauth::resolve_handle(http, resolver, handle).await?;
            (did, collection.clone(), rkey.clone())
        }
    };
    let at_uri = match (&collection, &rkey) {
        (Some(collection), Some(rkey)) => format!("at://{authority}/{collection}/{rkey}"),
        _ => format!("at://{authority}"),
    };
    // Account labels carry the bare DID as their uri; record labels the URI.
    let pattern = if collection.is_some() {
        at_uri.clone()
    } else {
        authority.clone()
    };
    Ok(ResolvedSubject {
        at_uri,
        pattern,
        collection,
    })
}

/// The labeler service endpoint from a DID document's `#atproto_labeler`
/// service (type `AtprotoLabeler`) — the same extraction shape as the PDS's
/// `#atproto_pds` service in [`crate::oauth::pds_endpoint`].
pub fn labeler_endpoint(did_doc: &serde_json::Value) -> Result<String> {
    did_doc["service"]
        .as_array()
        .and_then(|services| {
            services.iter().find(|service| {
                service["id"] == "#atproto_labeler" && service["type"] == "AtprotoLabeler"
            })
        })
        .and_then(|service| service["serviceEndpoint"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            AtmosphericError::Message(
                "no #atproto_labeler service in the DID document (not a labeler)".into(),
            )
        })
}

/// `GET` a URL and parse the body as JSON, with a readable error on failure.
async fn get_json(http: &reqwest::Client, url: &str) -> Result<serde_json::Value> {
    let response = http
        .get(url)
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    let status = response.status();
    let body = response.text().await.map_err(AtmosphericError::protocol)?;
    if !status.is_success() {
        return Err(AtmosphericError::Message(format!(
            "GET {url} failed with HTTP {status}: {body}"
        )));
    }
    Ok(serde_json::from_str(&body)?)
}

/// The `labels` array of an AppView view, reduced to digests.
fn labels_of(view: &serde_json::Value) -> Vec<LabelDigest> {
    view["labels"]
        .as_array()
        .map(|labels| labels.iter().filter_map(LabelDigest::from_json).collect())
        .unwrap_or_default()
}

/// Read the labels the AppView attaches to a subject's view. Accounts go
/// through `app.bsky.actor.getProfile`, posts through `app.bsky.feed.getPosts`,
/// lists through `app.bsky.graph.getList` (first page — the list view itself
/// carries the labels). Unauthenticated on the public AppView.
pub async fn view_labels(
    http: &reqwest::Client,
    appview: &str,
    subject: &ResolvedSubject,
) -> Result<Vec<LabelDigest>> {
    let base = appview.trim_end_matches('/');
    match subject.collection.as_deref() {
        None => {
            let url = format!("{base}/xrpc/app.bsky.actor.getProfile");
            let actor = subject
                .at_uri
                .strip_prefix("at://")
                .unwrap_or(&subject.at_uri);
            let view = get_json(http, &format!("{url}?actor={actor}")).await?;
            Ok(labels_of(&view))
        }
        Some(collection @ ("app.bsky.feed.post" | "app.bsky.graph.list")) => {
            let (endpoint, param) = if collection == "app.bsky.feed.post" {
                ("app.bsky.feed.getPosts", "uris")
            } else {
                ("app.bsky.graph.getList", "list")
            };
            let url = format!("{base}/xrpc/{endpoint}");
            let mut request = http.get(&url).query(&[(param, &subject.at_uri)]);
            if collection == "app.bsky.graph.list" {
                request = request.query(&[("limit", "1")]);
            }
            let response = request.send().await.map_err(AtmosphericError::protocol)?;
            let status = response.status();
            let body = response.text().await.map_err(AtmosphericError::protocol)?;
            if !status.is_success() {
                return Err(AtmosphericError::Message(format!(
                    "{endpoint} for {} failed with HTTP {status}: {body}",
                    subject.at_uri
                )));
            }
            let page: serde_json::Value = serde_json::from_str(&body)?;
            if collection == "app.bsky.feed.post" {
                let post = page["posts"]
                    .as_array()
                    .and_then(|posts| posts.first())
                    .ok_or_else(|| {
                        AtmosphericError::Message(format!(
                            "getPosts returned no post for {}",
                            subject.at_uri
                        ))
                    })?;
                Ok(labels_of(post))
            } else {
                Ok(labels_of(&page["list"]))
            }
        }
        Some(other) => Err(AtmosphericError::Message(format!(
            "the AppView has no view carrying labels for {other}; query a labeler instead (/atmospheric labeler <did> [subject])"
        ))),
    }
}

/// Query a labeler's `com.atproto.label.queryLabels` (unauthenticated),
/// paging with `limit` until the cursor ends. `patterns` are exact subject
/// URIs/DIDs or trailing-`*` prefixes.
pub async fn query_labels(
    http: &reqwest::Client,
    endpoint: &str,
    patterns: &[String],
) -> Result<Vec<LabelDigest>> {
    let base = endpoint.trim_end_matches('/');
    let mut labels: Vec<LabelDigest> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let limit = QUERY_LIMIT.to_string();
        let mut query: Vec<(&str, &str)> = patterns
            .iter()
            .map(|pattern| ("uriPatterns", pattern.as_str()))
            .collect();
        query.push(("limit", limit.as_str()));
        if let Some(cursor) = &cursor {
            query.push(("cursor", cursor));
        }
        let response = http
            .get(format!("{base}/xrpc/com.atproto.label.queryLabels"))
            .query(&query)
            .send()
            .await
            .map_err(AtmosphericError::protocol)?;
        let status = response.status();
        let body = response.text().await.map_err(AtmosphericError::protocol)?;
        if !status.is_success() {
            return Err(AtmosphericError::Message(format!(
                "queryLabels at {base} failed with HTTP {status}: {body}"
            )));
        }
        let page: serde_json::Value = serde_json::from_str(&body)?;
        labels.extend(
            page["labels"]
                .as_array()
                .map(|ls| ls.iter().filter_map(LabelDigest::from_json))
                .into_iter()
                .flatten(),
        );
        pages += 1;
        match page["cursor"].as_str() {
            Some(next) if !next.is_empty() && pages < MAX_PAGES => cursor = Some(next.to_string()),
            _ => break,
        }
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subject_references() {
        assert_eq!(
            parse_subject_reference("at://did:plc:alice/app.bsky.feed.post/3abc").unwrap(),
            SubjectRef::AtUri("at://did:plc:alice/app.bsky.feed.post/3abc".into())
        );
        assert_eq!(
            parse_subject_reference("at://did:plc:alice").unwrap(),
            SubjectRef::AtUri("at://did:plc:alice".into())
        );
        assert_eq!(
            parse_subject_reference("did:plc:alice").unwrap(),
            SubjectRef::Did("did:plc:alice".into())
        );
        assert_eq!(
            parse_subject_reference("alice.test").unwrap(),
            SubjectRef::NeedsResolve {
                handle: "alice.test".into(),
                collection: None,
                rkey: None
            }
        );
        assert_eq!(
            parse_subject_reference("https://bsky.app/profile/did:plc:alice/post/3abc").unwrap(),
            SubjectRef::AtUri("at://did:plc:alice/app.bsky.feed.post/3abc".into())
        );
        assert_eq!(
            parse_subject_reference("https://bsky.app/profile/alice.test/lists/3abc").unwrap(),
            SubjectRef::NeedsResolve {
                handle: "alice.test".into(),
                collection: Some("app.bsky.graph.list".into()),
                rkey: Some("3abc".into())
            }
        );
        assert_eq!(
            parse_subject_reference("https://bsky.app/profile/alice.test").unwrap(),
            SubjectRef::NeedsResolve {
                handle: "alice.test".into(),
                collection: None,
                rkey: None
            }
        );
        assert!(parse_subject_reference("at://did:plc:alice/app.bsky.feed.post").is_err());
        assert!(parse_subject_reference("https://example.com/x").is_err());
        assert!(parse_subject_reference("not a handle!").is_err());
        assert!(parse_subject_reference("   ").is_err());
    }

    #[test]
    fn extracts_the_labeler_service_endpoint() {
        let doc = serde_json::json!({
            "id": "did:plc:labeler",
            "service": [
                {"id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": "https://pds.example"},
                {"id": "#atproto_labeler", "type": "AtprotoLabeler", "serviceEndpoint": "https://labels.example"}
            ]
        });
        assert_eq!(labeler_endpoint(&doc).unwrap(), "https://labels.example");
        let no_labeler = serde_json::json!({"id": "did:plc:x", "service": []});
        assert!(labeler_endpoint(&no_labeler).is_err());
    }

    /// A stub HTTP server answering the label endpoints; returns the base
    /// URL and the recorded request paths.
    fn stub_server() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
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
                        let payload = if path.starts_with("/xrpc/app.bsky.actor.getProfile") {
                            r#"{"did":"did:plc:alice","handle":"alice.test","labels":[{"ver":1,"src":"did:plc:labeler","uri":"did:plc:alice","val":"player","cts":"2026-11-12T10:00:00.000Z"}]}"#
                        } else if path.starts_with("/xrpc/app.bsky.feed.getPosts") {
                            r#"{"posts":[{"uri":"at://did:plc:alice/app.bsky.feed.post/3abc","labels":[{"ver":1,"src":"did:plc:labeler","uri":"at://did:plc:alice/app.bsky.feed.post/3abc","val":"spam","neg":true,"cts":"2026-11-12T11:00:00.000Z","exp":"2026-12-12T11:00:00.000Z"}]}]}"#
                        } else if path.starts_with("/xrpc/app.bsky.graph.getList") {
                            r#"{"list":{"uri":"at://did:plc:alice/app.bsky.graph.list/3abc","labels":[{"ver":1,"src":"did:plc:labeler","uri":"at://did:plc:alice/app.bsky.graph.list/3abc","val":"curated","cts":"2026-11-12T09:00:00.000Z"}]},"items":[]}"#
                        } else if path.starts_with("/xrpc/com.atproto.label.queryLabels") {
                            if path.contains("cursor=2") {
                                r#"{"labels":[{"ver":1,"src":"did:plc:labeler","uri":"did:plc:alice","val":"online","cts":"2026-11-12T12:00:00.000Z"}]}"#
                            } else {
                                r#"{"labels":[{"ver":1,"src":"did:plc:labeler","uri":"did:plc:alice","val":"player","cts":"2026-11-12T10:00:00.000Z"}],"cursor":"2"}"#
                            }
                        } else {
                            r#"{"error":"UnknownEndpoint"}"#
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
        (url, hits)
    }

    #[test]
    fn view_labels_routes_by_collection() {
        let (url, hits) = stub_server();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let http = reqwest::Client::new();
        runtime.block_on(async {
            let account = ResolvedSubject {
                at_uri: "at://did:plc:alice".into(),
                pattern: "did:plc:alice".into(),
                collection: None,
            };
            let labels = view_labels(&http, &url, &account).await.unwrap();
            assert_eq!(labels.len(), 1);
            assert_eq!(labels[0].val, "player");
            assert_eq!(labels[0].src, "did:plc:labeler");
            assert!(!labels[0].neg);

            let post = ResolvedSubject {
                at_uri: "at://did:plc:alice/app.bsky.feed.post/3abc".into(),
                pattern: "at://did:plc:alice/app.bsky.feed.post/3abc".into(),
                collection: Some("app.bsky.feed.post".into()),
            };
            let labels = view_labels(&http, &url, &post).await.unwrap();
            assert_eq!(labels.len(), 1);
            assert_eq!(labels[0].val, "spam");
            assert!(labels[0].neg);
            assert_eq!(labels[0].exp.as_deref(), Some("2026-12-12T11:00:00.000Z"));

            let list = ResolvedSubject {
                at_uri: "at://did:plc:alice/app.bsky.graph.list/3abc".into(),
                pattern: "at://did:plc:alice/app.bsky.graph.list/3abc".into(),
                collection: Some("app.bsky.graph.list".into()),
            };
            let labels = view_labels(&http, &url, &list).await.unwrap();
            assert_eq!(labels.len(), 1);
            assert_eq!(labels[0].val, "curated");

            let other = ResolvedSubject {
                at_uri: "at://did:plc:alice/at.atpcraft.player.chat/3abc".into(),
                pattern: "at://did:plc:alice/at.atpcraft.player.chat/3abc".into(),
                collection: Some("at.atpcraft.player.chat".into()),
            };
            let error = view_labels(&http, &url, &other).await.unwrap_err();
            assert!(
                error.to_string().contains("at.atpcraft.player.chat"),
                "{error}"
            );
        });
        let hits = hits.lock().unwrap();
        assert!(hits[0].starts_with("/xrpc/app.bsky.actor.getProfile?actor=did:plc:alice"));
        assert!(hits[1].starts_with("/xrpc/app.bsky.feed.getPosts?uris="));
        assert!(hits[2].starts_with("/xrpc/app.bsky.graph.getList?list="));
        assert!(hits[2].contains("limit=1"));
    }

    #[test]
    fn query_labels_paginates_until_the_cursor_ends() {
        let (url, hits) = stub_server();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let labels = runtime.block_on(async {
            let http = reqwest::Client::new();
            query_labels(&http, &url, &["did:plc:alice".to_string()])
                .await
                .unwrap()
        });
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].val, "player");
        assert_eq!(labels[1].val, "online");
        let hits = hits.lock().unwrap();
        assert_eq!(hits.len(), 2, "two pages fetched: {hits:?}");
        assert!(hits[0].contains("uriPatterns=did%3Aplc%3Aalice"));
        assert!(hits[0].contains("limit=100"));
        assert!(!hits[0].contains("cursor="));
        assert!(hits[1].contains("cursor=2"));
    }
}
