//! Publishing interactions on Bluesky posts: replies, likes and reposts
//! (M21).
//!
//! The three write lexicons need strong references (`{uri, cid}`), and the
//! caller usually has only the target post's AT-URI. So the flow is: resolve
//! the author's PDS through the same directory service as the other reads,
//! fetch the target record unauthenticated (`com.atproto.repo.getRecord`) to
//! obtain its CID, derive the thread root for replies (the target's own
//! `reply.root` when it is itself a reply, otherwise the target), and then
//! write the interaction to your own repo (`com.atproto.repo.createRecord`).
//!
//! Record shapes come from the public Bluesky lexicons:
//! `app.bsky.feed.like` and `app.bsky.feed.repost` carry a `subject` strong
//! ref; `app.bsky.feed.post` carries `reply: { root, parent }`.

use crate::error::{AtmosphericError, Result};
use crate::oauth::{pds_endpoint, resolve_did_document};
use atrium_api::types::string::Datetime;
use serde_json::{Value, json};

/// The collection of a post, and of a reply (replies are posts).
pub const POST_COLLECTION: &str = "app.bsky.feed.post";
/// The like record collection.
pub const LIKE_COLLECTION: &str = "app.bsky.feed.like";
/// The repost record collection.
pub const REPOST_COLLECTION: &str = "app.bsky.feed.repost";

/// A strong reference: a record's AT-URI and its CID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrongRef {
    pub uri: String,
    pub cid: String,
}

impl StrongRef {
    /// The wire shape: `{"uri": "...", "cid": "..."}`.
    fn to_json(&self) -> Value {
        json!({ "uri": self.uri, "cid": self.cid })
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            uri: value["uri"].as_str()?.to_string(),
            cid: value["cid"].as_str()?.to_string(),
        })
    }
}

/// A resolved post target: its AT-URI, the author's DID and their PDS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostTarget {
    pub uri: String,
    pub did: String,
    pub pds: String,
}

/// A reply's two strong refs: the immediate parent and the thread root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyTargets {
    pub parent: StrongRef,
    pub root: StrongRef,
}

/// Split an `at://<did>/app.bsky.feed.post/<rkey>` URI into its repo and rkey.
fn split_post_uri(uri: &str) -> Result<(&str, &str)> {
    let rest = uri
        .strip_prefix("at://")
        .ok_or_else(|| AtmosphericError::Message(format!("not an at:// post URI: {uri}")))?;
    let mut parts = rest.splitn(3, '/');
    let did = parts.next().filter(|s| !s.is_empty()).unwrap_or_default();
    let collection = parts.next().unwrap_or_default();
    let rkey = parts.next().filter(|s| !s.is_empty()).unwrap_or_default();
    if did.is_empty() || rkey.is_empty() || collection != POST_COLLECTION {
        return Err(AtmosphericError::Message(format!(
            "want an at://<did>/{POST_COLLECTION}/<rkey> URI (got: {uri})"
        )));
    }
    Ok((did, rkey))
}

/// Resolve a post subject to its AT-URI, author DID and PDS. Accepts an
/// AT-URI or a bsky.app post URL; handles resolve through the directory
/// service. A non-post record URI is rejected before any PDS read.
pub async fn resolve_post(
    http: &reqwest::Client,
    resolver: &crate::oauth::Resolver,
    raw: &str,
) -> Result<PostTarget> {
    let reference = crate::labels::parse_subject_reference(raw)?;
    let resolved = crate::labels::resolve_subject(http, resolver, &reference).await?;
    if resolved.collection.as_deref() != Some(POST_COLLECTION) {
        return Err(AtmosphericError::Message(
            "want a post (at://…/app.bsky.feed.post/<rkey> or a bsky.app post URL)".into(),
        ));
    }
    let uri = resolved.pattern;
    let did = uri
        .strip_prefix("at://")
        .unwrap_or(&uri)
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let doc = resolve_did_document(http, resolver, &did).await?;
    let pds = pds_endpoint(&doc)?;
    Ok(PostTarget { uri, did, pds })
}

/// Fetch a post's record unauthenticated from `pds`: returns its strong ref
/// (URI + CID) and its value, so replies can pick up the thread root.
pub async fn fetch_ref(http: &reqwest::Client, pds: &str, uri: &str) -> Result<(StrongRef, Value)> {
    let (did, rkey) = split_post_uri(uri)?;
    let response = http
        .get(format!(
            "{}/xrpc/com.atproto.repo.getRecord",
            pds.trim_end_matches('/')
        ))
        .query(&[
            ("repo", did),
            ("collection", POST_COLLECTION),
            ("rkey", rkey),
        ])
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    let status = response.status();
    let body = response.text().await.map_err(AtmosphericError::protocol)?;
    if !status.is_success() {
        return Err(AtmosphericError::Message(format!(
            "getRecord for {uri} failed with HTTP {status}: {body}"
        )));
    }
    let value: Value = serde_json::from_str(&body)?;
    let strong = StrongRef::from_json(&value)
        .ok_or_else(|| AtmosphericError::Message(format!("getRecord for {uri} had no cid")))?;
    Ok((strong, value.get("value").cloned().unwrap_or(Value::Null)))
}

/// Derive a reply's root from the parent's value: a reply to a reply carries
/// its thread's `reply.root`; a top-level parent is its own root.
pub fn reply_targets(parent: StrongRef, value: &Value) -> ReplyTargets {
    let root = value
        .get("reply")
        .and_then(|reply| reply.get("root"))
        .and_then(StrongRef::from_json)
        .unwrap_or_else(|| parent.clone());
    ReplyTargets { parent, root }
}

/// The `app.bsky.feed.post` record for a plain-text reply.
pub fn reply_record(text: &str, targets: &ReplyTargets) -> Value {
    json!({
        "$type": POST_COLLECTION,
        "text": text,
        "createdAt": Datetime::now(),
        "reply": {
            "root": targets.root.to_json(),
            "parent": targets.parent.to_json(),
        },
    })
}

/// The `app.bsky.feed.like` record for a strong-ref subject.
pub fn like_record(target: &StrongRef) -> Value {
    json!({
        "$type": LIKE_COLLECTION,
        "subject": target.to_json(),
        "createdAt": Datetime::now(),
    })
}

/// The `app.bsky.feed.repost` record for a strong-ref subject.
pub fn repost_record(target: &StrongRef) -> Value {
    json!({
        "$type": REPOST_COLLECTION,
        "subject": target.to_json(),
        "createdAt": Datetime::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_server() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                std::thread::spawn(move || {
                    use std::io::{Read, Write};
                    let mut chunk = [0u8; 4096];
                    let _ = stream.read(&mut chunk);
                    let payload = r#"{"uri":"at://did:plc:target/app.bsky.feed.post/target1","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy","value":{"$type":"app.bsky.feed.post","text":"parent","reply":{"root":{"uri":"at://did:plc:root/app.bsky.feed.post/root1","cid":"bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy"},"parent":{"uri":"at://did:plc:root/app.bsky.feed.post/parent1","cid":"bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy"}}}}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });
        url
    }

    #[test]
    fn splits_only_post_uris() {
        assert_eq!(
            split_post_uri("at://did:plc:alice/app.bsky.feed.post/3abc").unwrap(),
            ("did:plc:alice", "3abc")
        );
        assert!(
            split_post_uri("at://did:plc:alice/app.bsky.feed.like/3abc").is_err(),
            "a like is not a post"
        );
        assert!(split_post_uri("at://did:plc:alice/app.bsky.feed.post").is_err());
        assert!(split_post_uri("https://bsky.app/profile/a/post/1").is_err());
    }

    #[test]
    fn reply_targets_inherit_the_thread_root_or_use_the_parent() {
        let parent = StrongRef {
            uri: "at://did:plc:target/app.bsky.feed.post/2".into(),
            cid: "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy".into(),
        };
        let top_level = serde_json::json!({"$type": POST_COLLECTION, "text": "root"});
        let targets = reply_targets(parent.clone(), &top_level);
        assert_eq!(targets.parent, parent);
        assert_eq!(targets.root, parent, "a top-level parent is its own root");

        let nested = serde_json::json!({
            "reply": {"root": {"uri": "at://did:plc:root/app.bsky.feed.post/1", "cid": "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy"}}
        });
        let targets = reply_targets(parent.clone(), &nested);
        assert_eq!(targets.parent, parent);
        assert_eq!(targets.root.uri, "at://did:plc:root/app.bsky.feed.post/1");
    }

    #[test]
    fn record_shapes_match_the_lexicons() {
        let target = StrongRef {
            uri: "at://did:plc:target/app.bsky.feed.post/1".into(),
            cid: "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy".into(),
        };
        let like = like_record(&target);
        assert_eq!(like["$type"], LIKE_COLLECTION);
        assert_eq!(like["subject"]["uri"], target.uri);
        assert_eq!(like["subject"]["cid"], target.cid);
        assert!(like["createdAt"].as_str().is_some());

        let repost = repost_record(&target);
        assert_eq!(repost["$type"], REPOST_COLLECTION);
        assert_eq!(repost["subject"]["uri"], target.uri);

        let targets = ReplyTargets {
            parent: target.clone(),
            root: target.clone(),
        };
        let reply = reply_record("nice", &targets);
        assert_eq!(reply["$type"], POST_COLLECTION);
        assert_eq!(reply["text"], "nice");
        assert_eq!(reply["reply"]["parent"]["cid"], target.cid);
        assert_eq!(reply["reply"]["root"]["uri"], target.uri);
    }

    #[test]
    fn fetch_ref_reads_the_record_and_derives_the_root() {
        let url = stub_server();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let http = reqwest::Client::new();
            let (parent, value) = fetch_ref(
                &http,
                &url,
                "at://did:plc:target/app.bsky.feed.post/target1",
            )
            .await
            .unwrap();
            assert_eq!(parent.uri, "at://did:plc:target/app.bsky.feed.post/target1");
            let targets = reply_targets(parent, &value);
            assert_eq!(
                targets.root.uri,
                "at://did:plc:root/app.bsky.feed.post/root1"
            );
        });
    }

    #[test]
    fn resolve_post_rejects_non_post_subjects_before_any_read() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let http = reqwest::Client::new();
            let resolver = crate::oauth::Resolver::default();
            for subject in [
                "at://did:plc:alice/app.bsky.feed.like/3abc",
                "at://did:plc:alice/app.bsky.graph.list/3abc",
                "did:plc:alice",
            ] {
                let error = resolve_post(&http, &resolver, subject).await.unwrap_err();
                assert!(error.to_string().contains("want a post"), "{error}");
            }
        });
    }
}
