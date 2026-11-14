//! Claiming and reading server-side identity links (M36/M37).
//!
//! The optional `atmospheric-server` bound the Minecraft UUID when it issued a
//! pairing code. The client only has to send the code and the logged-in
//! player's DID/handle; it never sees an admin token or another player's
//! credentials. A join uses the public side of the same map to resolve a UUID
//! before checking that player's Colibri profile.

use crate::error::{AtmosphericError, Result};
use serde::{Deserialize, Serialize};

const CLAIM_PATH: &str = "/v1/pairings/claim";
const LINKS_PATH: &str = "/v1/links";
const BROWSER_LINK_PATH: &str = "/v1/browser/pairings";

#[derive(Debug, Serialize)]
struct ClaimRequest<'a> {
    code: &'a str,
    did: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    handle: Option<&'a str>,
}

/// The one-time browser URL returned for an online player (M39).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BrowserLink {
    pub url: String,
    pub expires_at: u64,
}

#[derive(Debug, Serialize)]
struct BrowserLinkRequest<'a> {
    player_uuid: &'a str,
    player_name: &'a str,
    ttl_seconds: u64,
}

/// The link returned by the identity service after a successful claim.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClaimedLink {
    pub player_uuid: String,
    pub player_name: String,
    pub did: String,
    pub handle: Option<String>,
    pub linked_at: u64,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: Option<String>,
}

fn service_url(base: &str, path: &str) -> Result<String> {
    let base = base.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(AtmosphericError::Message(
            "server_api_url must start with http:// or https://".into(),
        ));
    }
    Ok(format!("{base}{path}"))
}

fn valid_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn service_error(status: reqwest::StatusCode, text: &str) -> String {
    serde_json::from_str::<ErrorBody>(text)
        .ok()
        .and_then(|body| body.error)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| format!("HTTP {status}"))
}

/// Build and validate the claim URL from the configured service base.
pub fn claim_url(base: &str) -> Result<String> {
    service_url(base, CLAIM_PATH)
}

/// Build and validate a public link lookup URL. The UUID is the canonical
/// lowercase hyphenated form the game-side hook produces.
pub fn link_url(base: &str, player_uuid: &str) -> Result<String> {
    if !valid_uuid(player_uuid) {
        return Err(AtmosphericError::Message(
            "player_uuid must be a canonical hyphenated Minecraft UUID".into(),
        ));
    }
    service_url(base, &format!("{LINKS_PATH}/{player_uuid}"))
}

/// Resolve a player's link through the public side of the M35 service (M37).
/// A 404 means the player has not linked an account and is not an error.
pub async fn fetch_link(
    http: &reqwest::Client,
    base: &str,
    player_uuid: &str,
) -> Result<Option<ClaimedLink>> {
    let url = link_url(base, player_uuid)?;
    let response = http
        .get(url)
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    let status = response.status();
    let text = response.text().await.map_err(AtmosphericError::protocol)?;
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(AtmosphericError::Message(format!(
            "server identity service: {}",
            service_error(status, &text)
        )));
    }
    let link: ClaimedLink = serde_json::from_str(&text)?;
    if link.player_uuid != player_uuid {
        return Err(AtmosphericError::Message(
            "server identity service returned a different player UUID".into(),
        ));
    }
    if link.did.trim().is_empty() || link.player_name.trim().is_empty() {
        return Err(AtmosphericError::Message(
            "server identity service returned an incomplete link".into(),
        ));
    }
    Ok(Some(link))
}

/// Ask the identity service for a one-time browser link URL (M39). The game
/// server authenticates with its shared link token; the returned URL is shown
/// only to that player. The UUID is not sent by the browser and cannot be
/// chosen by it.
pub async fn start_browser_link(
    http: &reqwest::Client,
    base: &str,
    link_token: &str,
    player_uuid: &str,
    player_name: &str,
    ttl_seconds: u64,
) -> Result<BrowserLink> {
    if link_token.trim().is_empty() {
        return Err(AtmosphericError::Message(
            "server_link_token is not configured".into(),
        ));
    }
    if !valid_uuid(player_uuid) {
        return Err(AtmosphericError::Message(
            "player_uuid must be a canonical hyphenated Minecraft UUID".into(),
        ));
    }
    if player_name.trim().is_empty() {
        return Err(AtmosphericError::Message("player_name is empty".into()));
    }
    let url = service_url(base, BROWSER_LINK_PATH)?;
    let response = http
        .post(url)
        .bearer_auth(link_token.trim())
        .json(&BrowserLinkRequest {
            player_uuid,
            player_name: player_name.trim(),
            ttl_seconds,
        })
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    let status = response.status();
    let text = response.text().await.map_err(AtmosphericError::protocol)?;
    if !status.is_success() {
        return Err(AtmosphericError::Message(format!(
            "server identity service: {}",
            service_error(status, &text)
        )));
    }
    let link: BrowserLink = serde_json::from_str(&text)?;
    if !(link.url.starts_with("http://") || link.url.starts_with("https://"))
        || link.expires_at == 0
    {
        return Err(AtmosphericError::Message(
            "server identity service returned an invalid browser link".into(),
        ));
    }
    Ok(link)
}

/// Claim a pairing code as `did`. The service owns the code-to-UUID binding,
/// so no Minecraft UUID is sent from the client.
pub async fn claim(
    http: &reqwest::Client,
    base: &str,
    code: &str,
    did: &str,
    handle: Option<&str>,
) -> Result<ClaimedLink> {
    let code = code.trim();
    if code.is_empty() {
        return Err(AtmosphericError::Message("pairing code is empty".into()));
    }
    if did.trim().is_empty() {
        return Err(AtmosphericError::NotLoggedIn);
    }
    let url = claim_url(base)?;
    let response = http
        .post(url)
        .json(&ClaimRequest {
            code,
            did,
            handle: handle.filter(|handle| !handle.trim().is_empty()),
        })
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    let status = response.status();
    let text = response.text().await.map_err(AtmosphericError::protocol)?;
    if !status.is_success() {
        return Err(AtmosphericError::Message(format!(
            "server identity service: {}",
            service_error(status, &text)
        )));
    }
    let link: ClaimedLink = serde_json::from_str(&text)?;
    if link.did != did {
        return Err(AtmosphericError::Message(
            "server identity service returned a different DID".into(),
        ));
    }
    if link.player_uuid.is_empty() || link.player_name.is_empty() {
        return Err(AtmosphericError::Message(
            "server identity service returned an incomplete link".into(),
        ));
    }
    Ok(link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
    };

    struct Stub {
        url: String,
        requests: Arc<Mutex<Vec<(String, String)>>>,
    }

    fn start(status: &str, payload: &str) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        {
            let requests = Arc::clone(&requests);
            let status = status.to_string();
            let payload = payload.to_string();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let requests = Arc::clone(&requests);
                    let status = status.clone();
                    let payload = payload.clone();
                    std::thread::spawn(move || {
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 4096];
                        let header_end = loop {
                            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                break pos;
                            }
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        };
                        let headers = String::from_utf8_lossy(&buf[..header_end]);
                        let request_line = headers.lines().next().unwrap_or_default();
                        let path = request_line.split_whitespace().nth(1).unwrap_or("").into();
                        let mut content_length = 0usize;
                        for line in headers.lines() {
                            if let Some(value) =
                                line.to_ascii_lowercase().strip_prefix("content-length:")
                            {
                                content_length = value.trim().parse().unwrap_or(0);
                            }
                        }
                        let mut body = buf[header_end + 4..].to_vec();
                        while body.len() < content_length {
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => body.extend_from_slice(&chunk[..n]),
                            }
                        }
                        requests
                            .lock()
                            .unwrap()
                            .push((path, String::from_utf8_lossy(&body).to_string()));
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    });
                }
            });
        }
        Stub { url, requests }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn claim_posts_code_did_and_handle() {
        let stub = start(
            "200 OK",
            r#"{"player_uuid":"00000000-0000-4000-8000-000000000001","player_name":"Alex","did":"did:plc:alice","handle":"alice.test","linked_at":7}"#,
        );
        let link = runtime()
            .block_on(claim(
                &reqwest::Client::new(),
                &format!("{}/", stub.url),
                " ABCD-EFGH ",
                "did:plc:alice",
                Some("alice.test"),
            ))
            .unwrap();
        assert_eq!(link.player_name, "Alex");
        assert_eq!(link.player_uuid, "00000000-0000-4000-8000-000000000001");
        assert_eq!(link.did, "did:plc:alice");
        assert_eq!(link.handle.as_deref(), Some("alice.test"));
        let requests = stub.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "/v1/pairings/claim");
        let body: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
        assert_eq!(body["code"], "ABCD-EFGH");
        assert_eq!(body["did"], "did:plc:alice");
        assert_eq!(body["handle"], "alice.test");
    }

    #[test]
    fn claim_surfaces_the_service_error() {
        let stub = start(
            "404 Not Found",
            r#"{"error":"pairing code is invalid, expired, or already used"}"#,
        );
        let error = runtime()
            .block_on(claim(
                &reqwest::Client::new(),
                &stub.url,
                "ABCD-EFGH",
                "did:plc:alice",
                None,
            ))
            .unwrap_err();
        assert!(error.to_string().contains("pairing code is invalid"));
        let body: serde_json::Value =
            serde_json::from_str(&stub.requests.lock().unwrap()[0].1).unwrap();
        assert!(body.get("handle").is_none(), "no handle when unknown");
    }

    #[test]
    fn claim_rejects_a_different_did() {
        let stub = start(
            "200 OK",
            r#"{"player_uuid":"u","player_name":"Alex","did":"did:plc:mallory","handle":null,"linked_at":7}"#,
        );
        let error = runtime()
            .block_on(claim(
                &reqwest::Client::new(),
                &stub.url,
                "ABCD-EFGH",
                "did:plc:alice",
                None,
            ))
            .unwrap_err();
        assert!(error.to_string().contains("different DID"));
    }

    #[test]
    fn claim_rejects_a_non_http_base() {
        let error = claim_url("file:///tmp/identity").unwrap_err();
        assert!(error.to_string().contains("server_api_url"));
    }

    #[test]
    fn browser_link_posts_the_uuid_name_and_lifetime() {
        let stub = start(
            "201 Created",
            r#"{"url":"https://links.example/link/token","expires_at":1800}"#,
        );
        let link = runtime()
            .block_on(start_browser_link(
                &reqwest::Client::new(),
                &format!("{}/", stub.url),
                "server-link-secret",
                "00000000-0000-4000-8000-000000000001",
                "Alex",
                900,
            ))
            .unwrap();
        assert_eq!(link.url, "https://links.example/link/token");
        assert_eq!(link.expires_at, 1800);
        let requests = stub.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "/v1/browser/pairings");
        let body: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
        assert_eq!(body["player_uuid"], "00000000-0000-4000-8000-000000000001");
        assert_eq!(body["player_name"], "Alex");
        assert_eq!(body["ttl_seconds"], 900);
    }

    #[test]
    fn browser_link_requires_its_shared_token() {
        let error = runtime()
            .block_on(start_browser_link(
                &reqwest::Client::new(),
                "https://links.example",
                "",
                "00000000-0000-4000-8000-000000000001",
                "Alex",
                600,
            ))
            .unwrap_err();
        assert!(error.to_string().contains("server_link_token"));
    }

    #[test]
    fn fetch_link_reads_the_public_map() {
        let stub = start(
            "200 OK",
            r#"{"player_uuid":"00000000-0000-4000-8000-000000000001","player_name":"Alex","did":"did:plc:alice","handle":"alice.test","linked_at":7}"#,
        );
        let link = runtime()
            .block_on(fetch_link(
                &reqwest::Client::new(),
                &format!("{}/", stub.url),
                "00000000-0000-4000-8000-000000000001",
            ))
            .unwrap()
            .expect("a linked player");
        assert_eq!(link.did, "did:plc:alice");
        let requests = stub.requests.lock().unwrap();
        assert_eq!(
            requests[0].0,
            "/v1/links/00000000-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn fetch_link_treats_a_missing_link_as_unlinked() {
        let stub = start("404 Not Found", r#"{"error":"no link"}"#);
        let link = runtime()
            .block_on(fetch_link(
                &reqwest::Client::new(),
                &stub.url,
                "00000000-0000-4000-8000-000000000001",
            ))
            .unwrap();
        assert!(link.is_none());
    }

    #[test]
    fn fetch_link_surfaces_the_service_error() {
        let stub = start("503 Service Unavailable", r#"{"error":"disabled"}"#);
        let error = runtime()
            .block_on(fetch_link(
                &reqwest::Client::new(),
                &stub.url,
                "00000000-0000-4000-8000-000000000001",
            ))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("server identity service: disabled")
        );
    }

    #[test]
    fn a_link_lookup_needs_a_canonical_uuid() {
        let error = link_url("http://127.0.0.1:1", "not-a-uuid").unwrap_err();
        assert!(error.to_string().contains("canonical"));
    }
}
