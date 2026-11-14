use super::{OAuthClient, Resolver};
use std::sync::{Arc, Mutex};

type Captured = Arc<Mutex<Vec<(String, String, String)>>>;

fn oauth_stub() -> (String, Captured) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let base = url.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let seen = Arc::clone(&seen);
            let base = base.clone();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let end = loop {
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos;
                    }
                    let n = match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&chunk[..n]);
                };
                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                let line = head.lines().next().unwrap_or_default();
                let mut words = line.split_whitespace();
                let method = words.next().unwrap_or_default().to_string();
                let target = words.next().unwrap_or_default().to_string();
                let path = target.split('?').next().unwrap_or_default().to_string();
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut body = buf[end + 4..].to_vec();
                while body.len() < length {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body.extend_from_slice(&chunk[..n]),
                    }
                }
                let body = String::from_utf8_lossy(&body).to_string();
                seen.lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), body.clone()));
                let payload = match (method.as_str(), path.as_str()) {
                    ("GET", "/xrpc/com.atproto.identity.resolveHandle") => {
                        r#"{"did":"did:plc:stubuser"}"#.to_string()
                    }
                    ("GET", "/stubuser") => format!(
                        r##"{{"service":[{{"id":"#atproto_pds","serviceEndpoint":"{base}"}}]}}"##
                    ),
                    ("GET", "/.well-known/oauth-protected-resource") => format!(
                        r#"{{"authorization_servers":["{base}"]}}"#
                    ),
                    ("GET", "/.well-known/oauth-authorization-server") => format!(
                        r#"{{"issuer":"{base}","authorization_endpoint":"{base}/authorize","token_endpoint":"{base}/token","pushed_authorization_request_endpoint":"{base}/par"}}"#
                    ),
                    ("POST", "/par") => {
                        r#"{"request_uri":"urn:atmospheric:test:req1","expires_in":60}"#.to_string()
                    }
                    ("POST", "/token") => {
                        r#"{"access_token":"access","token_type":"DPoP","sub":"did:plc:stubuser","scope":"atproto transition:generic"}"#
                            .to_string()
                    }
                    _ => r#"{"error":"not found"}"#.to_string(),
                };
                let status = if payload.contains("\"error\"") {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes());
            });
        }
    });
    (url, requests)
}

#[test]
fn browser_oauth_start_and_finish_against_a_stub() {
    let (base, requests) = oauth_stub();
    let client = OAuthClient::new(
        "https://links.example",
        Resolver {
            handle_service: base.clone(),
            plc_directory: base.clone(),
        },
    );
    let started = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(client.start("Stub.Test"))
        .unwrap();
    assert!(
        started
            .authorize_url
            .starts_with(&format!("{base}/authorize?"))
    );
    assert!(
        started
            .authorize_url
            .contains("request_uri=urn%3Aatmospheric%3Atest%3Areq1")
    );
    assert_eq!(started.pending.did, "did:plc:stubuser");
    assert_eq!(started.pending.issuer, base);

    let identity = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(client.finish(&started.pending, "code", Some(&base)))
        .unwrap();
    assert_eq!(identity.did, "did:plc:stubuser");
    assert_eq!(identity.handle, "stub.test");

    let requests = requests.lock().unwrap();
    let par = requests
        .iter()
        .find(|(method, path, _)| method == "POST" && path == "/par")
        .unwrap();
    assert!(par.2.contains("code_challenge_method=S256"));
    assert!(par.2.contains("dpop_jkt="));
    assert!(
        par.2
            .contains("client_id=https%3A%2F%2Flinks.example%2Foauth-client-metadata.json")
    );
    let token = requests
        .iter()
        .find(|(method, path, _)| method == "POST" && path == "/token")
        .unwrap();
    assert!(token.2.contains("grant_type=authorization_code"));
    assert!(token.2.contains("code_verifier="));
}

#[test]
fn browser_oauth_rejects_a_mismatched_issuer() {
    let (base, _) = oauth_stub();
    let client = OAuthClient::new(
        "https://links.example",
        Resolver {
            handle_service: base.clone(),
            plc_directory: base.clone(),
        },
    );
    let started = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(client.start("stub.test"))
        .unwrap();
    let error = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(client.finish(&started.pending, "code", Some("https://other.example")))
        .unwrap_err();
    assert!(error.contains("does not match"));
}
