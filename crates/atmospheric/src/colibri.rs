//! Colibri chat bridge protocol (M26).
//!
//! This module is pure protocol plus the worker-side WebSocket loop. The game
//! side only sees commands and events; no socket, HTTP client or serde type
//! crosses the runtime boundary.

use crate::{
    actions,
    error::{AtmosphericError, Result},
    session::Agent,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::SEC_WEBSOCKET_PROTOCOL},
    },
};

/// The channel record collection.
pub const CHANNEL_COLLECTION: &str = "social.colibri.channel";
/// The community record collection.
pub const COMMUNITY_COLLECTION: &str = "social.colibri.community";
/// The message record collection.
pub const MESSAGE_COLLECTION: &str = "social.colibri.message";
/// The membership record collection.
pub const MEMBERSHIP_COLLECTION: &str = "social.colibri.membership";
/// The AppView's event-stream method.
pub const SUBSCRIBE_EVENTS_LXM: &str = "social.colibri.sync.subscribeEvents";
/// The service id of the AppView in its DID document.
pub const APPVIEW_SERVICE_ID: &str = "colibri_appview";
/// The Colibri message text limit from the lexicon.
pub const MAX_TEXT_LENGTH: usize = 2048;

const COMMUNITY_SELF: &str = "self";
const DEEP_LINK_SCHEME: &str = "social.colibri:";
const HEARTBEAT_SECS: u64 = 30;
const MAX_BACKOFF_SECS: u64 = 60;
/// Rotate the one-hour service-auth token before it expires.
pub const TOKEN_ROTATION_SECS: u64 = 45 * 60;

/// One bridged channel, parsed from an AT-URI or a Colibri app link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRef {
    pub channel_uri: String,
    pub community_did: String,
    pub rkey: String,
    pub community_rkey: String,
}

impl ChannelRef {
    /// Parse a channel AT-URI, a web link or the native app's deep link.
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return None;
        }
        Self::parse_at_uri(trimmed).or_else(|| Self::parse_link(trimmed))
    }

    fn parse_at_uri(value: &str) -> Option<Self> {
        let rest = value.strip_prefix("at://")?;
        let (authority, path) = rest.split_once('/')?;
        let mut parts = path.split('/');
        if parts.next()? != CHANNEL_COLLECTION {
            return None;
        }
        let rkey = parts.next()?;
        if parts.next().is_some() || !valid_did(authority) || !valid_rkey(rkey) {
            return None;
        }
        Some(Self {
            channel_uri: value.to_string(),
            community_did: authority.to_string(),
            rkey: rkey.to_string(),
            community_rkey: COMMUNITY_SELF.to_string(),
        })
    }

    fn parse_link(value: &str) -> Option<Self> {
        let without_query = value.split(['?', '#']).next().unwrap_or(value);
        let lower = without_query.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            let after_scheme = without_query.split_once("://")?.1;
            let segments: Vec<&str> = after_scheme.split('/').filter(|s| !s.is_empty()).collect();
            // The first segment is the host; the app path starts after it.
            let path = segments.get(1..)?;
            if path.len() < 5 || path[0] != "app" || path[1] != "c" {
                return None;
            }
            Self::from_link_parts(path[2], path[3], path[4])
        } else if lower.starts_with(DEEP_LINK_SCHEME) {
            let path = &without_query[DEEP_LINK_SCHEME.len()..];
            let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if segments.len() < 4 || segments[0] != "channel" {
                return None;
            }
            Self::from_link_parts(segments[1], segments[2], segments[3])
        } else {
            None
        }
    }

    fn from_link_parts(community: &str, channel_type: &str, rkey: &str) -> Option<Self> {
        if !valid_channel_type(channel_type) {
            return None;
        }
        let (did, community_rkey) = split_community(community)?;
        let rkey = percent_decode(rkey)?;
        if !valid_rkey(&rkey) {
            return None;
        }
        Some(Self {
            channel_uri: format!("at://{did}/{CHANNEL_COLLECTION}/{rkey}"),
            community_did: did,
            rkey,
            community_rkey,
        })
    }

    /// AT-URI of the community record that owns this channel.
    pub fn community_uri(&self) -> String {
        format!(
            "at://{}/{COMMUNITY_COLLECTION}/{}",
            self.community_did, self.community_rkey
        )
    }
}

fn valid_did(value: &str) -> bool {
    value.starts_with("did:")
        && value.len() > 4
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '%' | '-'))
}

fn valid_rkey(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | ':' | '-'))
}

fn valid_channel_type(value: &str) -> bool {
    matches!(value, "text" | "voice" | "forum" | "link")
        || (value.starts_with("social.colibri.channel.")
            && value["social.colibri.channel.".len()..]
                .chars()
                .all(|c| c.is_ascii_lowercase()))
}

fn split_community(segment: &str) -> Option<(String, String)> {
    if let Some(rest) = segment.strip_prefix("did:plc:")
        && let Some(hyphen) = rest.find('-')
    {
        let did = format!("did:plc:{}", &rest[..hyphen]);
        let rkey = &rest[hyphen + 1..];
        if valid_did(&did) && valid_rkey(rkey) {
            return Some((did, rkey.to_string()));
        }
        return None;
    }
    if valid_did(segment) {
        Some((segment.to_string(), COMMUNITY_SELF.to_string()))
    } else {
        None
    }
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = *bytes.get(i + 1)?;
            let lo = *bytes.get(i + 2)?;
            out.push((hex(hi)? << 4) | hex(lo)?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The audience the PDS puts in the service-auth token.
pub fn appview_service_ref(did: &str) -> String {
    let trimmed = did.trim();
    if trimmed.contains('#') || trimmed.is_empty() {
        trimmed.to_string()
    } else {
        format!("{trimmed}#{APPVIEW_SERVICE_ID}")
    }
}

/// A fresh RFC 3339 timestamp, in the shape the lexicons expect.
pub fn now() -> String {
    atrium_api::types::string::Datetime::now()
        .as_str()
        .to_string()
}

/// `social.colibri.message` body.
pub fn message_record(text: &str, channel_uri: &str, created_at: &str) -> Value {
    json!({
        "$type": MESSAGE_COLLECTION,
        "text": text.chars().take(MAX_TEXT_LENGTH).collect::<String>(),
        "channel": channel_uri,
        "createdAt": created_at,
    })
}

/// `social.colibri.membership` body.
pub fn membership_record(community_uri: &str, created_at: &str) -> Value {
    json!({
        "$type": MEMBERSHIP_COLLECTION,
        "community": community_uri,
        "createdAt": created_at,
    })
}

/// Ensure the server account has a membership record for the channel's
/// community. Returns `true` when a record was created.
///
/// The AppView only sends events to community members. atpcraft uses
/// `createRecord` (not `putRecord`) because an OAuth grant for this collection
/// carries create/delete, not update.
pub async fn ensure_membership(
    agent: &Agent,
    did: &str,
    channel: &ChannelRef,
    rkey: &str,
) -> Result<bool> {
    use atrium_api::com::atproto::repo::list_records;
    let community = channel.community_uri();
    let params = list_records::ParametersData {
        collection: MEMBERSHIP_COLLECTION.parse().expect("valid NSID"),
        cursor: None,
        limit: Some(100.try_into().expect("valid list limit")),
        repo: did
            .parse()
            .map_err(|_| AtmosphericError::Message(format!("invalid DID: {did}")))?,
        reverse: None,
    };
    let page = agent
        .api
        .com
        .atproto
        .repo
        .list_records(params.into())
        .await
        .map_err(AtmosphericError::protocol)?;
    let already_member = page.data.records.iter().any(|record| {
        serde_json::to_value(&record.value)
            .ok()
            .and_then(|value| value.get("community")?.as_str().map(str::to_owned))
            .as_deref()
            == Some(community.as_str())
    });
    if already_member {
        return Ok(false);
    }
    actions::create_record_json(
        agent,
        did,
        MEMBERSHIP_COLLECTION,
        Some(rkey),
        membership_record(&community, &now()),
    )
    .await?;
    Ok(true)
}

/// Mint the inter-service token the AppView accepts for the event stream.
pub async fn service_auth(agent: &Agent, aud: &str, lxm: &str) -> Result<String> {
    use atrium_api::com::atproto::server::get_service_auth;
    let params = get_service_auth::ParametersData {
        aud: aud.parse().map_err(|_| {
            AtmosphericError::Message(format!("invalid Colibri service audience: {aud}"))
        })?,
        exp: None,
        lxm: Some(
            lxm.parse()
                .map_err(|_| AtmosphericError::Message(format!("invalid Colibri method: {lxm}")))?,
        ),
    };
    let output = agent
        .api
        .com
        .atproto
        .server
        .get_service_auth(params.into())
        .await
        .map_err(AtmosphericError::protocol)?;
    Ok(output.data.token)
}

/// The WebSocket URL of the AppView's event stream.
pub fn stream_url(appview_url: &str) -> String {
    let base = appview_url.trim().trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{base}/xrpc/{SUBSCRIBE_EVENTS_LXM}")
}

/// The `Sec-WebSocket-Protocol` value: sentinel first, token second.
pub fn subprotocol_header(token: &str) -> String {
    format!("colibri.auth.bearer, {token}")
}

/// One message from the event stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    pub uri: String,
    pub channel: String,
    pub text: String,
    pub author_did: String,
    pub author_handle: String,
}

/// What the socket loop reports to the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Connected,
    Disconnected(String),
    Rotate,
    Message(IncomingMessage),
}

/// Decode one text frame. Unknown and malformed frames are ignored.
pub fn decode_event(frame: &str) -> Option<StreamEvent> {
    let root: Value = serde_json::from_str(frame).ok()?;
    let kind = root.get("type")?.as_str()?;
    if kind != "message_event" {
        return None;
    }
    let data = root.get("data")?;
    let uri = data.get("uri")?.as_str()?.to_string();
    match data.get("event").and_then(Value::as_str) {
        Some("delete") => None,
        Some("upsert") => {
            let author = data.get("author");
            let did = author
                .and_then(|a| a.get("did"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let raw_handle = author
                .and_then(|a| a.get("handle"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let handle = raw_handle
                .trim()
                .strip_prefix("at://")
                .unwrap_or(raw_handle.trim())
                .trim_end_matches('/');
            Some(StreamEvent::Message(IncomingMessage {
                uri,
                channel: data.get("channel")?.as_str()?.to_string(),
                text: data
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                author_did: did.clone(),
                author_handle: if handle.is_empty() {
                    did
                } else {
                    handle.to_string()
                },
            }))
        }
        _ => None,
    }
}

/// Run the event stream until the worker aborts the task.
///
/// The token is minted once by the worker and reused across reconnects. atpcraft
/// rotates the one-hour token every 45 minutes; proactive rotation is remaining
/// M26+ work, so a reconnect after that window needs `/atmospheric colibri stop`
/// followed by `start`.
pub async fn run_stream(appview_url: String, token: String, tx: UnboundedSender<StreamEvent>) {
    let url = stream_url(&appview_url);
    let mut backoff = 1u64;
    loop {
        match connect_once(&url, &token, &tx).await {
            Ok(true) => {
                let _ = tx.send(StreamEvent::Rotate);
                return;
            }
            Ok(false) => {
                backoff = 1;
                if tx
                    .send(StreamEvent::Disconnected("stream closed".into()))
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                if tx.send(StreamEvent::Disconnected(error)).is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
    }
}

async fn connect_once(
    url: &str,
    token: &str,
    tx: &UnboundedSender<StreamEvent>,
) -> std::result::Result<bool, String> {
    let mut request = url
        .into_client_request()
        .map_err(|error| format!("invalid Colibri stream URL: {error}"))?;
    let protocol = HeaderValue::from_str(&subprotocol_header(token))
        .map_err(|error| format!("invalid Colibri auth subprotocol: {error}"))?;
    request
        .headers_mut()
        .insert(SEC_WEBSOCKET_PROTOCOL, protocol);
    let (mut socket, _) = connect_async(request)
        .await
        .map_err(|error| format!("Colibri stream connection failed: {error}"))?;
    if tx.send(StreamEvent::Connected).is_err() {
        return Ok(false);
    }
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(HEARTBEAT_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let rotation = tokio::time::sleep(std::time::Duration::from_secs(TOKEN_ROTATION_SECS));
    tokio::pin!(rotation);
    loop {
        tokio::select! {
            _ = &mut rotation => return Ok(true),
            _ = heartbeat.tick() => {
                socket
                    .send(Message::Text("{\"type\":\"heartbeat\"}".into()))
                    .await
                    .map_err(|error| format!("Colibri heartbeat failed: {error}"))?;
            }
            frame = socket.next() => {
                let Some(frame) = frame else {
                    return Err("Colibri stream ended".into());
                };
                let frame = frame.map_err(|error| format!("Colibri stream error: {error}"))?;
                match frame {
                    Message::Text(text) => {
                        if let Some(event) = decode_event(&text)
                            && tx.send(event).is_err()
                        {
                            return Ok(false);
                        }
                    }
                    Message::Ping(payload) => {
                        socket
                            .send(Message::Pong(payload))
                            .await
                            .map_err(|error| format!("Colibri pong failed: {error}"))?;
                    }
                    Message::Close(_) => return Ok(false),
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_at_uri_and_links_parse_to_the_same_channel() {
        let expected = ChannelRef {
            channel_uri: "at://did:plc:abc/social.colibri.channel/3jtest".into(),
            community_did: "did:plc:abc".into(),
            rkey: "3jtest".into(),
            community_rkey: "self".into(),
        };
        for input in [
            "at://did:plc:abc/social.colibri.channel/3jtest",
            "https://colibri.social/app/c/did:plc:abc/text/3jtest",
            "https://self-hosted.example/app/c/did:plc:abc/voice/3jtest?x=1#y",
            "social.colibri:/channel/did:plc:abc/text/3jtest",
        ] {
            assert_eq!(ChannelRef::parse(input), Some(expected.clone()), "{input}");
        }
        let old = ChannelRef::parse("https://colibri.social/app/c/did:plc:abc-old/text/3jtest")
            .expect("old community link");
        assert_eq!(old.community_rkey, "old");
        assert_eq!(
            old.community_uri(),
            "at://did:plc:abc/social.colibri.community/old"
        );
        assert!(ChannelRef::parse("https://colibri.social/app/c/did:plc:abc/text").is_none());
        assert!(ChannelRef::parse("at://did:plc:abc/app.bsky.feed.post/3jtest").is_none());
    }

    #[test]
    fn appview_audience_and_stream_url_are_built_correctly() {
        assert_eq!(
            appview_service_ref("did:web:api.colibri.social"),
            "did:web:api.colibri.social#colibri_appview"
        );
        assert_eq!(
            appview_service_ref("did:web:api.colibri.social#custom"),
            "did:web:api.colibri.social#custom"
        );
        assert_eq!(
            stream_url("https://api.colibri.social/"),
            "wss://api.colibri.social/xrpc/social.colibri.sync.subscribeEvents"
        );
        assert_eq!(subprotocol_header("tok"), "colibri.auth.bearer, tok");
    }

    #[test]
    fn message_and_membership_records_match_the_lexicons() {
        let message = message_record(
            "hello",
            "at://did:plc:abc/social.colibri.channel/1",
            "2026-01-01T00:00:00Z",
        );
        assert_eq!(message["$type"], MESSAGE_COLLECTION);
        assert_eq!(message["text"], "hello");
        assert_eq!(
            message["channel"],
            "at://did:plc:abc/social.colibri.channel/1"
        );
        let long = "x".repeat(MAX_TEXT_LENGTH + 10);
        assert_eq!(
            message_record(&long, "c", "t")["text"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            MAX_TEXT_LENGTH
        );
        let membership = membership_record(
            "at://did:plc:abc/social.colibri.community/self",
            "2026-01-01T00:00:00Z",
        );
        assert_eq!(membership["$type"], MEMBERSHIP_COLLECTION);
        assert_eq!(
            membership["community"],
            "at://did:plc:abc/social.colibri.community/self"
        );
    }

    // The tungstenite handshake callback owns its fixed Response error type;
    // boxing it is not possible without changing the server API.
    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn stream_connects_and_delivers_a_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                socket,
                |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    response.headers_mut().insert(
                        SEC_WEBSOCKET_PROTOCOL,
                        HeaderValue::from_static("colibri.auth.bearer"),
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
            socket
                .send(Message::Text(
                    r#"{"type":"message_event","data":{"uri":"at://did:plc:alice/social.colibri.message/1","channel":"at://did:plc:abc/social.colibri.channel/1","event":"upsert","text":"live","author":{"did":"did:plc:alice","handle":"alice.test"}}}"#
                        .into(),
                ))
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let stream = tokio::spawn(run_stream(format!("ws://{addr}"), "token".into(), tx));
        let connected = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("the stream connects")
            .expect("a connection event");
        assert_eq!(connected, StreamEvent::Connected);
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("the event arrives")
            .expect("a message event");
        match message {
            StreamEvent::Message(message) => {
                assert_eq!(message.text, "live");
                assert_eq!(message.author_handle, "alice.test");
            }
            other => panic!("expected a message, got {other:?}"),
        }
        stream.abort();
        server.abort();
    }

    #[test]
    fn decode_event_reads_upserts_and_ignores_other_frames() {
        let frame = r#"{
            "type": "message_event",
            "data": {
                "uri": "at://did:plc:alice/social.colibri.message/3j1",
                "channel": "at://did:plc:abc/social.colibri.channel/1",
                "event": "upsert",
                "text": "hello",
                "author": {"did": "did:plc:alice", "handle": "at://alice.test"}
            }
        }"#;
        assert_eq!(
            decode_event(frame),
            Some(StreamEvent::Message(IncomingMessage {
                uri: "at://did:plc:alice/social.colibri.message/3j1".into(),
                channel: "at://did:plc:abc/social.colibri.channel/1".into(),
                text: "hello".into(),
                author_did: "did:plc:alice".into(),
                author_handle: "alice.test".into(),
            }))
        );
        assert!(decode_event(r#"{"type":"ack"}"#).is_none());
        assert!(decode_event("not json").is_none());
        assert!(decode_event(r#"{"type":"message_event","data":{"event":"delete"}}"#).is_none());
    }
}
