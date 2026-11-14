//! AT Protocol OAuth with a loopback redirect (M8), replacing app passwords.
//!
//! The flow follows the atproto OAuth profile (https://atproto.com/specs/oauth):
//! resolve handle → DID → DID document → PDS, discover the PDS's authorization
//! server, push an authorization request (PAR, RFC 9126) with PKCE S256 and a
//! DPoP key thumbprint, open the browser at the authorize URL, receive the
//! redirect on a loopback listener (`http://127.0.0.1:<port>/`), and exchange
//! the code at the token endpoint with a DPoP proof. Access tokens are
//! DPoP-bound: every request carries `Authorization: DPoP <token>` plus a `DPoP`
//! proof JWT (ES256) — that part lives in `account/session.rs` (`DpopClient`), which
//! reuses [`DpopKey`] from here.
//!
//! The mod registers as a loopback "development" client: `client_id` is
//! `http://localhost?client_id=atmospheric-horizon&scope=…`, which the spec
//! allows without publishing client metadata on the public web. Wire formats
//! and flow are reimplemented from the public spec (informed by atpcraft's
//! public description of its flow); no code is copied from any source.

use crate::error::{AtmosphericError, Result};
use base64::Engine as _;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The scope requested on the consent screen. `atproto` is mandatory; the
/// transitional `transition:generic` grant is the OAuth equivalent of the old
/// app-password level (repo writes, blob uploads, PDS API calls) and is what
/// third-party PDS authorization servers reliably support today.
pub const DEFAULT_SCOPE: &str = "atproto transition:generic";

/// Add the Colibri grants to the OAuth scope when the bridge is enabled.
///
/// The PDS only mints a service-auth token when the session carries the
/// matching `rpc:` grant, and Colibri message/membership records need their
/// repo grants. Without the bridge the scope is unchanged, so ordinary logins
/// request no more than before.
pub fn scope_for_colibri(bridge_enabled: bool, appview_did: &str) -> String {
    if !bridge_enabled {
        return DEFAULT_SCOPE.into();
    }
    let aud = crate::colibri::appview_service_ref(appview_did);
    format!(
        "{DEFAULT_SCOPE} \
         repo:social.colibri.message?action=create&action=delete \
         repo:social.colibri.membership?action=create&action=delete \
         rpc:social.colibri.sync.subscribeEvents?aud={aud}"
    )
}
/// The loopback redirect host (IP literal, as the spec requires for native
/// clients; `localhost` is reserved for the `client_id` origin).
pub const LOOPBACK_HOST: &str = "127.0.0.1";
/// How long the worker waits for the browser redirect before giving up.
pub const REDIRECT_WAIT: Duration = Duration::from_secs(300);

/// The shared DPoP state: the proof key plus the current tokens. Cloned into
/// the session store and the XRPC client so every request is signed and
/// authorised with the same material.
pub type SharedDpop = Arc<Mutex<Option<DpopState>>>;

pub fn new_shared_dpop() -> SharedDpop {
    Arc::new(Mutex::new(None))
}

/// base64url without padding (RFC 7515).
fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn b64url_decode(text: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|error| AtmosphericError::Message(format!("bad base64url: {error}")))
}

/// Percent-encode a query/form component (RFC 3986 unreserved set kept).
pub fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The loopback `client_id`: origin `http://localhost`, empty path, with the
/// declared scope as the `scope` query parameter. The spec's "virtual client
/// metadata document" for localhost clients builds the rest (native
/// application type, authorization_code + refresh_token grants,
/// `dpop_bound_access_tokens: true`, loopback redirect URIs) around it.
pub fn client_id(scope: &str) -> String {
    format!("http://localhost?scope={}", url_encode(scope))
}

/// A random URL-safe token (base64url of `bytes` random bytes).
pub fn random_token(bytes: usize) -> String {
    use p256::elliptic_curve::rand_core::{OsRng, RngCore};
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    b64url(&buf)
}

/// The PKCE code verifier (RFC 7636: 43–128 chars of unreserved characters).
pub fn pkce_verifier() -> String {
    random_token(32)
}

/// The PKCE S256 code challenge: base64url(SHA-256(verifier)).
pub fn pkce_challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

/// The DPoP `ath` claim: base64url(SHA-256(ASCII(access_token))) (RFC 9449 §6.1).
pub fn access_token_hash(access_token: &str) -> String {
    b64url(&Sha256::digest(access_token.as_bytes()))
}

/// Normalise a request URI to the DPoP `htu` value: scheme, host, port only
/// when non-default, and path — no query, no fragment (RFC 9449 §4.2).
pub fn normalize_htu(uri: &atrium_xrpc::http::Uri) -> String {
    let scheme = uri.scheme_str().unwrap_or("https");
    let host = uri.host().unwrap_or_default();
    let default_port = match scheme {
        "http" => 80,
        "https" => 443,
        _ => 0,
    };
    let port = uri.port_u16().filter(|p| *p != default_port);
    let path = uri.path();
    let path = if path.is_empty() { "/" } else { path };
    match port {
        Some(port) => format!("{scheme}://{host}:{port}{path}"),
        None => format!("{scheme}://{host}{path}"),
    }
}

/// Normalise a URL string the same way [`normalize_htu`] does.
pub fn normalize_url(url: &str) -> Result<String> {
    let uri: atrium_xrpc::http::Uri = url
        .parse()
        .map_err(|error| AtmosphericError::Message(format!("bad URL {url}: {error}")))?;
    Ok(normalize_htu(&uri))
}

/// An ES256 (P-256) key pair for DPoP proofs, persistable as a private JWK.
///
/// The private part is secret material: it lives only in `session.json` under
/// the mod's storage folder (never a world save) and is never logged.
#[derive(Clone)]
pub struct DpopKey {
    secret: p256::SecretKey,
}

impl DpopKey {
    pub fn generate() -> Self {
        Self {
            secret: p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng),
        }
    }

    pub fn signing_key(&self) -> SigningKey {
        SigningKey::from(&self.secret)
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        *self.signing_key().verifying_key()
    }

    /// The public JWK (`crv`/`kty`/`x`/`y`), as embedded in every proof's
    /// header.
    pub fn public_jwk(&self) -> serde_json::Value {
        let point = self.verifying_key().to_encoded_point(false);
        let bytes = point.as_bytes();
        serde_json::json!({
            "crv": "P-256",
            "kty": "EC",
            "x": b64url(&bytes[1..33]),
            "y": b64url(&bytes[33..65]),
        })
    }

    /// The RFC 7638 JWK thumbprint, sent as `dpop_jkt` in the PAR request.
    pub fn thumbprint(&self) -> String {
        let jwk = self.public_jwk();
        let canonical = format!(
            "{{\"crv\":\"{}\",\"kty\":\"{}\",\"x\":\"{}\",\"y\":\"{}\"}}",
            jwk["crv"].as_str().unwrap_or_default(),
            jwk["kty"].as_str().unwrap_or_default(),
            jwk["x"].as_str().unwrap_or_default(),
            jwk["y"].as_str().unwrap_or_default(),
        );
        b64url(&Sha256::digest(canonical.as_bytes()))
    }

    /// The private JWK (includes `d`), as persisted in `session.json`.
    pub fn to_jwk_json(&self) -> String {
        let mut jwk = self.public_jwk();
        jwk["d"] = serde_json::Value::String(b64url(&self.secret.to_bytes()));
        jwk.to_string()
    }

    /// Rebuild a key from the persisted private JWK.
    pub fn from_jwk_json(raw: &str) -> Result<Self> {
        let jwk: serde_json::Value = serde_json::from_str(raw)?;
        let d = jwk["d"].as_str().ok_or_else(|| {
            AtmosphericError::Message("stored DPoP JWK has no private `d`".into())
        })?;
        let secret = p256::SecretKey::from_slice(&b64url_decode(d)?).map_err(|error| {
            AtmosphericError::Message(format!("stored DPoP JWK is invalid: {error}"))
        })?;
        Ok(Self { secret })
    }

    /// A DPoP proof JWT (RFC 9449 §4.2): header `{typ: dpop+jwt, alg: ES256,
    /// jwk}`, claims `jti`/`htm`/`htu`/`iat` plus `nonce` and `ath` when given,
    /// signed ES256 with the raw (R‖S) signature encoding JWS requires.
    pub fn proof(
        &self,
        htm: &str,
        htu: &str,
        nonce: Option<&str>,
        ath: Option<&str>,
    ) -> Result<String> {
        let header = serde_json::json!({
            "typ": "dpop+jwt",
            "alg": "ES256",
            "jwk": self.public_jwk(),
        });
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut claims = serde_json::json!({
            "jti": random_token(16),
            "htm": htm,
            "htu": htu,
            "iat": now,
        });
        if let Some(nonce) = nonce {
            claims["nonce"] = serde_json::Value::String(nonce.into());
        }
        if let Some(ath) = ath {
            claims["ath"] = serde_json::Value::String(ath.into());
        }
        let head = b64url(&serde_json::to_vec(&header)?);
        let payload = b64url(&serde_json::to_vec(&claims)?);
        let signing_input = format!("{head}.{payload}");
        let signature: Signature = self.signing_key().sign(signing_input.as_bytes());
        Ok(format!("{signing_input}.{}", b64url(&signature.to_bytes())))
    }
}

/// The DPoP state shared between the worker, the session store and the XRPC
/// client once an OAuth login succeeds.
#[derive(Clone)]
pub struct DpopState {
    pub key: DpopKey,
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when the access token expires, if the AS said so.
    pub expires_at: Option<u64>,
    pub scope: String,
}

impl DpopState {
    pub fn from_tokens(key: DpopKey, tokens: &TokenSet) -> Self {
        Self {
            key,
            access_token: tokens.access_token.clone(),
            refresh_token: tokens.refresh_token.clone().unwrap_or_default(),
            expires_at: tokens.expires_in.map(|secs| unix_now() + secs),
            scope: tokens.scope.clone().unwrap_or_default(),
        }
    }

    /// Whether the access token is expired, or expires within `skew` seconds.
    pub fn expires_within(&self, skew: u64) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false; // no expiry information: assume it holds
        };
        unix_now() + skew >= expires_at
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where handles and DIDs resolve. Defaults are the public services; tests
/// point these at a stub.
#[derive(Debug, Clone)]
pub struct Resolver {
    pub handle_service: String,
    pub plc_directory: String,
}

impl Default for Resolver {
    fn default() -> Self {
        Self {
            handle_service: "https://bsky.social".into(),
            plc_directory: "https://plc.directory".into(),
        }
    }
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
    serde_json::from_str(&body).map_err(AtmosphericError::Serde)
}

/// Resolve a handle to its DID via `com.atproto.identity.resolveHandle`.
pub async fn resolve_handle(
    http: &reqwest::Client,
    resolver: &Resolver,
    handle: &str,
) -> Result<String> {
    let url = format!(
        "{}/xrpc/com.atproto.identity.resolveHandle",
        resolver.handle_service.trim_end_matches('/')
    );
    let response = http
        .get(&url)
        .query(&[("handle", handle)])
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    let status = response.status();
    let body = response.text().await.map_err(AtmosphericError::protocol)?;
    if !status.is_success() {
        return Err(AtmosphericError::Message(format!(
            "resolveHandle for {handle} failed with HTTP {status}: {body}"
        )));
    }
    let doc: serde_json::Value = serde_json::from_str(&body)?;
    doc["did"].as_str().map(str::to_owned).ok_or_else(|| {
        AtmosphericError::Message(format!("resolveHandle for {handle} returned no DID"))
    })
}

/// Fetch the DID document (did:plc via the PLC directory, did:web via the
/// domain's well-known document).
pub async fn resolve_did_document(
    http: &reqwest::Client,
    resolver: &Resolver,
    did: &str,
) -> Result<serde_json::Value> {
    let url = if did.starts_with("did:plc:") {
        format!("{}/{did}", resolver.plc_directory.trim_end_matches('/'))
    } else if let Some(domain) = did.strip_prefix("did:web:") {
        format!(
            "https://{}/.well-known/did.json",
            domain.replace(':', "%3A")
        )
    } else {
        return Err(AtmosphericError::Message(format!(
            "unsupported DID method: {did}"
        )));
    };
    get_json(http, &url).await
}

/// The PDS endpoint from the DID document's `#atproto_pds` service.
pub fn pds_endpoint(did_doc: &serde_json::Value) -> Result<String> {
    did_doc["service"]
        .as_array()
        .and_then(|services| {
            services.iter().find(|service| {
                service["id"] == "#atproto_pds" && service["type"] == "AtprotoPersonalDataServer"
            })
        })
        .and_then(|service| service["serviceEndpoint"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            AtmosphericError::Message("no #atproto_pds service in the DID document".into())
        })
}

/// The token endpoint's response (RFC 6749 plus the atproto-required `sub`
/// and `scope` fields).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub token_type: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// The account DID the token was issued for; must match the resolved DID.
    pub sub: String,
    #[serde(default)]
    pub scope: Option<String>,
}

impl TokenSet {
    /// The atproto profile requires DPoP-bound tokens, a `sub` matching the
    /// expected DID, and a granted scope containing `atproto`.
    fn validate(&self, expected_did: Option<&str>) -> Result<()> {
        if self.token_type != "DPoP" {
            return Err(AtmosphericError::Message(format!(
                "authorization server returned token_type {:?}, expected \"DPoP\"",
                self.token_type
            )));
        }
        if let Some(expected) = expected_did
            && self.sub != expected
        {
            return Err(AtmosphericError::Message(format!(
                "token was issued for {} but the login resolved {expected}",
                self.sub
            )));
        }
        if !self
            .scope
            .as_deref()
            .unwrap_or_default()
            .split(' ')
            .any(|s| s == "atproto")
        {
            return Err(AtmosphericError::Message(
                "granted scope does not contain the mandatory `atproto` scope".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct PdsMetadata {
    authorization_servers: Vec<String>,
}

#[derive(Deserialize)]
struct AuthServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    pushed_authorization_request_endpoint: String,
}

#[derive(Deserialize)]
struct ParResponse {
    request_uri: String,
}

/// A PAR-completed authorization: everything needed to finish the flow after
/// the browser redirect.
pub struct StartedAuthorization {
    pub authorize_url: String,
    pub state: String,
    pub code_verifier: String,
    pub key: DpopKey,
    pub client_id: String,
    pub redirect_uri: String,
    pub token_endpoint: String,
    pub issuer: String,
    pub pds_url: String,
    pub did: String,
    pub handle: String,
    /// The loopback listener the redirect arrives on.
    pub listener: tokio::net::TcpListener,
}

/// The finished login, handed back to the worker to build the agent.
pub struct OAuthLogin {
    pub tokens: TokenSet,
    pub key: DpopKey,
    pub pds_url: String,
    pub did: String,
    pub handle: String,
    pub token_endpoint: String,
    pub issuer: String,
    pub client_id: String,
    pub scope: String,
}

/// A response snapshot: status, headers and body, so the nonce-retry loop can
/// inspect (and, under `ATMOSPHERIC_DEBUG_HTTP`, print) every attempt.
#[derive(Debug, Clone)]
pub(crate) struct SimpleResponse {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub body: Vec<u8>,
}

impl SimpleResponse {
    pub fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Send one DPoP-protected request, retrying with the server's `DPoP-Nonce`
/// when it answers 400/401 with one (RFC 9449 §7, up to 5 attempts — servers
/// may rotate the nonce on every response).
async fn dpop_send(
    http: &reqwest::Client,
    key: &DpopKey,
    method: reqwest::Method,
    url: &str,
    form: Option<&[(&str, &str)]>,
    authorization: Option<&str>,
) -> Result<SimpleResponse> {
    let htu = normalize_url(url)?;
    let ath = authorization.map(access_token_hash);
    let mut nonce: Option<String> = None;
    for attempt in 0..5 {
        let proof = key.proof(method.as_str(), &htu, nonce.as_deref(), ath.as_deref())?;
        let mut request = http.request(method.clone(), url).header("DPoP", proof);
        if let Some(token) = authorization {
            request = request.header(reqwest::header::AUTHORIZATION, format!("DPoP {token}"));
        }
        if let Some(form) = form {
            request = request.form(form);
        }
        let response = request.send().await.map_err(AtmosphericError::protocol)?;
        let snapshot = SimpleResponse {
            status: response.status().as_u16(),
            headers: response.headers().clone(),
            body: response
                .bytes()
                .await
                .map_err(AtmosphericError::protocol)?
                .to_vec(),
        };
        if std::env::var("ATMOSPHERIC_DEBUG_HTTP").is_ok() && !snapshot.success() {
            eprintln!(
                "DEBUG {method} {url} attempt {attempt} -> {} nonce={:?} body={}",
                snapshot.status,
                snapshot.header("DPoP-Nonce"),
                snapshot.text()
            );
        }
        let is_challenge =
            matches!(snapshot.status, 400 | 401) && snapshot.headers.contains_key("DPoP-Nonce");
        if is_challenge {
            nonce = snapshot.header("DPoP-Nonce").map(str::to_owned);
            continue;
        }
        return Ok(snapshot);
    }
    Err(AtmosphericError::Message(
        "the authorization server kept challenging the DPoP nonce".into(),
    ))
}

/// Parse a token endpoint response body.
fn token_response(response: &SimpleResponse, url: &str) -> Result<TokenSet> {
    if !response.success() {
        return Err(AtmosphericError::Message(format!(
            "POST {url} failed with HTTP {}: {}",
            response.status,
            response.text()
        )));
    }
    serde_json::from_slice(&response.body).map_err(AtmosphericError::Serde)
}

/// The discovered authorization server endpoints for an account's PDS.
#[derive(Debug, Clone)]
pub struct AuthServer {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub par_endpoint: String,
}

/// Resolve a handle to its DID and PDS endpoint.
pub async fn resolve_account(
    http: &reqwest::Client,
    resolver: &Resolver,
    handle: &str,
) -> Result<(String, String)> {
    let did = resolve_handle(http, resolver, handle).await?;
    let did_doc = resolve_did_document(http, resolver, &did).await?;
    let pds_url = pds_endpoint(&did_doc)?;
    Ok((did, pds_url))
}

/// Discover the authorization server of a PDS via its well-known documents.
pub async fn discover_auth_server(http: &reqwest::Client, pds_url: &str) -> Result<AuthServer> {
    let pds_meta: PdsMetadata = serde_json::from_value(
        get_json(
            http,
            &format!(
                "{}/.well-known/oauth-protected-resource",
                pds_url.trim_end_matches('/')
            ),
        )
        .await?,
    )?;
    let auth_server = pds_meta
        .authorization_servers
        .first()
        .cloned()
        .ok_or_else(|| {
            AtmosphericError::Message(format!("no authorization server for PDS {pds_url}"))
        })?;
    let as_meta: AuthServerMetadata = serde_json::from_value(
        get_json(
            http,
            &format!(
                "{}/.well-known/oauth-authorization-server",
                auth_server.trim_end_matches('/')
            ),
        )
        .await?,
    )?;
    Ok(AuthServer {
        issuer: as_meta.issuer,
        authorization_endpoint: as_meta.authorization_endpoint,
        token_endpoint: as_meta.token_endpoint,
        par_endpoint: as_meta.pushed_authorization_request_endpoint,
    })
}

/// Push the authorization request (PAR, RFC 9126) at the authorization
/// server: the whole request form-encoded, with the DPoP key thumbprint so the
/// AS binds the session to this key. Returns the `request_uri`.
pub async fn push_par(
    http: &reqwest::Client,
    auth_server: &AuthServer,
    client_id: &str,
    scope: &str,
    login_hint: &str,
    redirect_uri: &str,
    key: &DpopKey,
) -> Result<String> {
    let code_verifier = pkce_verifier();
    let code_challenge = pkce_challenge(&code_verifier);
    let state = random_token(32);
    let jkt = key.thumbprint();
    let form: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("code_challenge", &code_challenge),
        ("code_challenge_method", "S256"),
        ("client_id", client_id),
        ("state", &state),
        ("redirect_uri", redirect_uri),
        ("scope", scope),
        ("login_hint", login_hint),
        ("dpop_jkt", &jkt),
    ];
    let response = dpop_send(
        http,
        key,
        reqwest::Method::POST,
        &auth_server.par_endpoint,
        Some(&form),
        None,
    )
    .await?;
    if !response.success() {
        return Err(AtmosphericError::Message(format!(
            "PAR failed with HTTP {}: {}",
            response.status,
            response.text()
        )));
    }
    let par: ParResponse = serde_json::from_slice(&response.body)?;
    Ok(par.request_uri)
}

/// Resolve the account, discover its PDS and authorization server, bind the
/// loopback listener, and push the authorization request (PAR). Returns the
/// authorize URL for the browser plus everything the redirect and token
/// exchange need.
///
/// `client_id` is normally the loopback form from [`client_id`]; set it to a
/// hosted client-metadata URL for authorization servers that do not implement
/// the loopback exception (see the README).
pub async fn start_authorization(
    http: &reqwest::Client,
    resolver: &Resolver,
    handle: &str,
    client_id: &str,
    scope: &str,
) -> Result<StartedAuthorization> {
    let (did, pds_url) = resolve_account(http, resolver, handle).await?;
    let auth_server = discover_auth_server(http, &pds_url).await?;

    // Bind the loopback listener first so the port is known for redirect_uri.
    let listener = tokio::net::TcpListener::bind((LOOPBACK_HOST, 0))
        .await
        .map_err(AtmosphericError::Io)?;
    let port = listener.local_addr().map_err(AtmosphericError::Io)?.port();
    let redirect_uri = format!("http://{LOOPBACK_HOST}:{port}/");

    let code_verifier = pkce_verifier();
    let code_challenge = pkce_challenge(&code_verifier);
    let state = random_token(32);
    let key = DpopKey::generate();
    let jkt = key.thumbprint();

    let form: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("code_challenge", &code_challenge),
        ("code_challenge_method", "S256"),
        ("client_id", client_id),
        ("state", &state),
        ("redirect_uri", &redirect_uri),
        ("scope", scope),
        ("login_hint", handle),
        ("dpop_jkt", &jkt),
    ];
    let response = dpop_send(
        http,
        &key,
        reqwest::Method::POST,
        &auth_server.par_endpoint,
        Some(&form),
        None,
    )
    .await?;
    if !response.success() {
        return Err(AtmosphericError::Message(format!(
            "PAR failed with HTTP {}: {}",
            response.status,
            response.text()
        )));
    }
    let par: ParResponse = serde_json::from_slice(&response.body)?;
    let authorize_url = format!(
        "{}?request_uri={}&client_id={}",
        auth_server.authorization_endpoint,
        url_encode(&par.request_uri),
        url_encode(client_id)
    );

    Ok(StartedAuthorization {
        authorize_url,
        state,
        code_verifier,
        key,
        client_id: client_id.to_owned(),
        redirect_uri,
        token_endpoint: auth_server.token_endpoint,
        issuer: auth_server.issuer,
        pds_url,
        did,
        handle: handle.to_owned(),
        listener,
    })
}

/// Wait on the loopback listener for the browser's redirect. Answers the
/// browser with a small HTML page either way, and returns the `code` once a
/// redirect with a matching `state` arrives. Other requests (favicon, …) are
/// answered and skipped.
pub async fn wait_for_redirect(
    authorization: &mut StartedAuthorization,
    timeout: Duration,
) -> Result<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(AtmosphericError::Message(
                "timed out waiting for the browser login redirect".into(),
            ));
        }
        let accepted = tokio::time::timeout(remaining, authorization.listener.accept())
            .await
            .map_err(|_| {
                AtmosphericError::Message("timed out waiting for the browser login redirect".into())
            })?
            .map_err(AtmosphericError::Io)?;
        let (mut socket, _) = accepted;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Read until the end of the request headers (a single read may return
        // only a fragment), bounded by the deadline and a size cap. Reading
        // the whole header block matters: closing a socket that still has
        // unread received data makes Linux send a RST, which can destroy the
        // response before the browser reads it.
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = tokio::time::timeout(remaining, socket.read(&mut chunk))
                .await
                .map_err(|_| AtmosphericError::Message("timed out reading the redirect".into()))?
                .map_err(AtmosphericError::Io)?;
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..read]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 8192 {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buf).to_string();
        let request_line = request.lines().next().unwrap_or_default();
        let target = request_line.split_whitespace().nth(1).unwrap_or("/");
        let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
        let params = parse_query(query);

        let (status, message) = if let Some(error) = params.get("error") {
            (
                "400 Bad Request",
                format!("the authorization server returned an error: {error}"),
            )
        } else if let Some(_code) = params.get("code") {
            match params.get("state") {
                Some(state) if *state == authorization.state => (
                    "200 OK",
                    "login complete — you can close this tab".to_string(),
                ),
                _ => {
                    let _ = socket
                        .write_all(
                            response(
                                "400 Bad Request",
                                "state mismatch — this redirect is not from your login",
                            )
                            .as_bytes(),
                        )
                        .await;
                    let _ = socket.shutdown().await;
                    return Err(AtmosphericError::Message(
                        "the redirect's state did not match the authorization request".into(),
                    ));
                }
            }
        } else {
            ("200 OK", "ok".to_string())
        };
        let _ = socket
            .write_all(response(status, &message).as_bytes())
            .await;
        let _ = socket.shutdown().await;
        // Drain whatever the client still sends so the close is a clean FIN
        // rather than a RST on Linux.
        let mut sink = [0u8; 4096];
        let _ = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut sink)).await;

        if let Some(code) = params.get("code")
            && params.get("state").map(|s| s.as_str()) == Some(authorization.state.as_str())
        {
            return Ok(code.clone());
        }
    }
}

fn response(status: &str, message: &str) -> String {
    let body = format!(
        "<!doctype html><html><body style=\"font-family:sans-serif;background:#111;color:#eee;padding:2em\"><h1>atmospheric</h1><p>{message}</p></body></html>"
    );
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Parse a query/form string into key/value pairs: form encoding, so `+` is
/// a space, then percent-decoded.
fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(
            url_decode(&key.replace('+', " ")),
            url_decode(&value.replace('+', " ")),
        );
    }
    out
}

fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &text[i + 1..i + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Exchange the redirect's code at the token endpoint (with PKCE verifier and
/// a DPoP proof), validating `sub`, token type and scope.
pub async fn exchange_code(
    http: &reqwest::Client,
    authorization: &StartedAuthorization,
    code: &str,
) -> Result<TokenSet> {
    let form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &authorization.redirect_uri),
        ("client_id", &authorization.client_id),
        ("code_verifier", &authorization.code_verifier),
    ];
    let response = dpop_send(
        http,
        &authorization.key,
        reqwest::Method::POST,
        &authorization.token_endpoint,
        Some(&form),
        None,
    )
    .await?;
    let tokens = token_response(&response, &authorization.token_endpoint)?;
    tokens.validate(Some(&authorization.did))?;
    Ok(tokens)
}

/// Refresh an access token (`grant_type=refresh_token`). The refresh token is
/// DPoP-bound, so the same key must sign the request.
pub async fn refresh(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &str,
    key: &DpopKey,
) -> Result<TokenSet> {
    let form: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    let response = dpop_send(
        http,
        key,
        reqwest::Method::POST,
        token_endpoint,
        Some(&form),
        None,
    )
    .await?;
    let tokens = token_response(&response, token_endpoint)?;
    // A refresh response may omit `sub`; when present it must still match.
    tokens.validate(None)?;
    Ok(tokens)
}

/// Try to open the system browser at the authorize URL. Returns `false` when
/// no browser could be launched; the authorize URL is always emitted as an
/// `Event::OAuthAuthorize` first, so the user can open it manually — the same
/// fallback Sequoia's CLI uses.
pub fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let (program, args): (&str, Vec<&str>) = ("open", vec![url]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let (program, args): (&str, Vec<&str>) = ("xdg-open", vec![url]);
    #[cfg(windows)]
    let (program, args): (&str, Vec<&str>) = ("cmd", vec!["/C", "start", "", url]);
    std::process::Command::new(program)
        .args(&args)
        .spawn()
        .is_ok()
}

/// Inputs for one browser login flow (see [`run_login`]).
pub struct LoginFlow<'a> {
    pub http: &'a reqwest::Client,
    pub resolver: &'a Resolver,
    pub handle: &'a str,
    pub client_id: &'a str,
    pub scope: &'a str,
    pub redirect_timeout: Duration,
    /// Whether to try spawning the system browser at the authorize URL.
    pub launch_browser: bool,
    pub events: &'a std::sync::mpsc::Sender<crate::Event>,
}

/// Run the whole browser flow: resolve + PAR, emit the authorize URL, wait for
/// the loopback redirect, exchange the code. Called from the worker's spawned
/// task so the command loop stays responsive while the browser is open.
pub async fn run_login(flow: LoginFlow<'_>) -> Result<OAuthLogin> {
    let LoginFlow {
        http,
        resolver,
        handle,
        client_id,
        scope,
        redirect_timeout,
        launch_browser,
        events,
    } = flow;
    let mut authorization = start_authorization(http, resolver, handle, client_id, scope).await?;
    // The URL always goes out first, so it is visible even when no browser
    // can be launched (headless game, missing xdg-open, …).
    let _ = events.send(crate::Event::OAuthAuthorize {
        url: authorization.authorize_url.clone(),
    });
    if launch_browser && !open_browser(&authorization.authorize_url) {
        let _ = events.send(crate::Event::Failed {
            message: format!(
                "could not open a browser automatically — open this link manually: {}",
                authorization.authorize_url
            ),
        });
    }
    let code = wait_for_redirect(&mut authorization, redirect_timeout).await?;
    let tokens = exchange_code(http, &authorization, &code).await?;
    Ok(OAuthLogin {
        tokens,
        key: authorization.key,
        pds_url: authorization.pds_url,
        did: authorization.did,
        handle: authorization.handle,
        token_endpoint: authorization.token_endpoint,
        issuer: authorization.issuer,
        client_id: authorization.client_id.clone(),
        scope: scope.to_owned(),
    })
}

#[cfg(test)]
pub(crate) mod tests;
