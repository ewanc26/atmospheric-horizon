use super::encoding::{normalize_base, url_encode};

/// Where handles and DIDs resolve. The defaults are Bluesky's public services;
/// tests point them at a stub.
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

pub(super) struct Endpoints {
    pub(super) issuer: String,
    pub(super) authorize_endpoint: String,
    pub(super) token_endpoint: String,
    pub(super) par_endpoint: String,
}

impl Resolver {
    pub(super) async fn resolve_handle(
        &self,
        http: &reqwest::Client,
        handle: &str,
    ) -> Result<String, String> {
        let base = normalize_base(&self.handle_service);
        let url = format!(
            "{base}/xrpc/com.atproto.identity.resolveHandle?handle={}",
            url_encode(handle)
        );
        let response = get_json(http, &url).await?;
        response
            .get("did")
            .and_then(|value| value.as_str())
            .filter(|did| did.starts_with("did:"))
            .map(str::to_owned)
            .ok_or_else(|| "the handle did not resolve to a DID".into())
    }

    pub(super) async fn resolve_pds(
        &self,
        http: &reqwest::Client,
        did: &str,
    ) -> Result<String, String> {
        let url = if let Some(rest) = did.strip_prefix("did:plc:") {
            format!("{}/{}", normalize_base(&self.plc_directory), rest)
        } else if let Some(domain) = did.strip_prefix("did:web:") {
            let mut parts = domain.split(':');
            let host = parts.next().unwrap_or_default();
            let path: Vec<&str> = parts.collect();
            if path.is_empty() {
                format!("https://{host}/.well-known/did.json")
            } else {
                format!("https://{host}/{}/did.json", path.join("/"))
            }
        } else {
            return Err("only did:plc and did:web accounts are supported".into());
        };
        let document = get_json(http, &url).await?;
        let services = document
            .get("service")
            .and_then(|value| value.as_array())
            .ok_or_else(|| "the DID document has no services".to_string())?;
        for service in services {
            if service.get("id").and_then(|value| value.as_str()) == Some("#atproto_pds") {
                let endpoint = service
                    .get("serviceEndpoint")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if endpoint.starts_with("https://") || endpoint.starts_with("http://") {
                    return Ok(normalize_base(endpoint));
                }
            }
        }
        Err("the DID document has no PDS service".into())
    }

    pub(super) async fn authorization_server(
        &self,
        http: &reqwest::Client,
        pds: &str,
    ) -> Result<Endpoints, String> {
        let protected = get_json(
            http,
            &format!(
                "{}/.well-known/oauth-protected-resource",
                normalize_base(pds)
            ),
        )
        .await?;
        let issuer = protected
            .get("authorization_servers")
            .and_then(|value| value.as_array())
            .and_then(|servers| servers.first())
            .and_then(|value| value.as_str())
            .filter(|issuer| !issuer.is_empty())
            .ok_or_else(|| "the PDS advertised no authorization server".to_string())?;
        let metadata = get_json(
            http,
            &format!(
                "{}/.well-known/oauth-authorization-server",
                normalize_base(issuer)
            ),
        )
        .await?;
        let field = |name: &str| {
            metadata
                .get(name)
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("authorization server metadata has no {name}"))
        };
        let issuer = metadata
            .get("issuer")
            .and_then(|value| value.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| issuer.to_string());
        Ok(Endpoints {
            issuer,
            authorize_endpoint: field("authorization_endpoint")?,
            token_endpoint: field("token_endpoint")?,
            par_endpoint: field("pushed_authorization_request_endpoint")?,
        })
    }
}

async fn get_json(http: &reqwest::Client, url: &str) -> Result<serde_json::Value, String> {
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|error| format!("GET {url} failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("GET {url} failed: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "GET {url} failed with HTTP {status}: {}",
            body.chars().take(300).collect::<String>()
        ));
    }
    serde_json::from_str(&body).map_err(|error| format!("GET {url} returned invalid JSON: {error}"))
}
