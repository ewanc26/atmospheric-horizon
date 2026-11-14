use super::{
    dpop::DpopKey,
    encoding::{normalize_base, pkce_challenge, random_token, url_encode},
    resolver::Resolver,
};

pub const SCOPE: &str = "atproto transition:generic";

/// The one-time material kept between PAR and the callback.
#[derive(Clone)]
pub struct PendingAuthorization {
    pub state: String,
    pub verifier: String,
    pub key: DpopKey,
    pub token_endpoint: String,
    pub issuer: String,
    pub did: String,
    pub handle: String,
}

pub struct Started {
    pub authorize_url: String,
    pub pending: PendingAuthorization,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub did: String,
    pub handle: String,
}

#[derive(Clone)]
pub struct OAuthClient {
    http: reqwest::Client,
    resolver: Resolver,
    client_id: String,
    redirect_uri: String,
}

impl OAuthClient {
    pub fn new(public_url: &str, resolver: Resolver) -> Self {
        let base = normalize_base(public_url);
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            resolver,
            client_id: format!("{base}/oauth-client-metadata.json"),
            redirect_uri: format!("{base}/link/callback"),
        }
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Resolve a handle and push an authorization request. The returned URL is
    /// the browser's next stop; `pending` belongs in the one-time session map.
    pub async fn start(&self, handle: &str) -> Result<Started, String> {
        let handle = handle.trim().trim_start_matches('@').to_ascii_lowercase();
        if handle.is_empty() || !handle.contains('.') {
            return Err("enter a Bluesky handle such as alice.bsky.social".into());
        }
        let did = self.resolver.resolve_handle(&self.http, &handle).await?;
        let pds = self.resolver.resolve_pds(&self.http, &did).await?;
        let endpoints = self.resolver.authorization_server(&self.http, &pds).await?;
        let state = random_token(24);
        let verifier = random_token(32);
        let key = DpopKey::generate();
        let form = vec![
            ("response_type".to_string(), "code".to_string()),
            ("client_id".to_string(), self.client_id.clone()),
            ("redirect_uri".to_string(), self.redirect_uri.clone()),
            ("scope".to_string(), SCOPE.to_string()),
            ("state".to_string(), state.clone()),
            ("code_challenge".to_string(), pkce_challenge(&verifier)),
            ("code_challenge_method".to_string(), "S256".to_string()),
            ("login_hint".to_string(), handle.clone()),
            ("dpop_jkt".to_string(), key.thumbprint()),
        ];
        let (response, _) = self
            .post_form(&endpoints.par_endpoint, &form, &key, None)
            .await
            .map_err(|error| format!("authorization request failed: {error}"))?;
        let request_uri = response
            .get("request_uri")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "authorization server returned no request_uri".to_string())?;
        let authorize_url = format!(
            "{}?client_id={}&request_uri={}",
            endpoints.authorize_endpoint,
            url_encode(&self.client_id),
            url_encode(request_uri)
        );
        Ok(Started {
            authorize_url,
            pending: PendingAuthorization {
                state,
                verifier,
                key,
                token_endpoint: endpoints.token_endpoint,
                issuer: endpoints.issuer,
                did,
                handle,
            },
        })
    }

    /// Exchange the callback code and validate the DPoP-bound token identity.
    pub async fn finish(
        &self,
        pending: &PendingAuthorization,
        code: &str,
        issuer: Option<&str>,
    ) -> Result<Identity, String> {
        let issuer = issuer
            .map(str::trim)
            .filter(|issuer| !issuer.is_empty())
            .ok_or_else(|| "the authorization response has no issuer".to_string())?;
        if normalize_base(issuer) != normalize_base(&pending.issuer) {
            return Err(
                "the authorization server does not match the one that started this login".into(),
            );
        }
        let form = vec![
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code".to_string(), code.to_string()),
            ("redirect_uri".to_string(), self.redirect_uri.clone()),
            ("client_id".to_string(), self.client_id.clone()),
            ("code_verifier".to_string(), pending.verifier.clone()),
        ];
        let (response, _) = self
            .post_form(&pending.token_endpoint, &form, &pending.key, None)
            .await
            .map_err(|error| format!("token exchange failed: {error}"))?;
        let token_type = response
            .get("token_type")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if token_type != "DPoP" {
            return Err("the authorization server did not return a DPoP token".into());
        }
        let did = response
            .get("sub")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if did != pending.did {
            return Err("the signed-in account did not match the requested handle".into());
        }
        let scope = response
            .get("scope")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if !scope.split_whitespace().any(|grant| grant == "atproto") {
            return Err("the authorization server did not grant the atproto scope".into());
        }
        Ok(Identity {
            did: did.to_string(),
            handle: pending.handle.clone(),
        })
    }

    async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
        key: &DpopKey,
        nonce: Option<&str>,
    ) -> Result<(serde_json::Value, Option<String>), String> {
        let proof = key.proof("POST", url, nonce, None)?;
        let response = self
            .http
            .post(url)
            .header("DPoP", proof)
            .form(form)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let next_nonce = response
            .headers()
            .get("DPoP-Nonce")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.text().await.map_err(|error| error.to_string())?;
        let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(
            |_| serde_json::json!({ "error": body.chars().take(300).collect::<String>() }),
        );
        if status == reqwest::StatusCode::BAD_REQUEST
            && value.get("error").and_then(|value| value.as_str()) == Some("use_dpop_nonce")
            && let Some(nonce) = next_nonce
        {
            return Box::pin(self.post_form(url, form, key, Some(&nonce))).await;
        }
        if !status.is_success() {
            return Err(format!(
                "HTTP {status}: {}",
                value
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or("authorization server refused the request")
            ));
        }
        Ok((value, next_nonce))
    }
}

/// The client metadata document at the URL used as the OAuth `client_id`.
pub fn client_metadata(public_url: &str) -> serde_json::Value {
    let base = normalize_base(public_url);
    serde_json::json!({
        "client_id": format!("{base}/oauth-client-metadata.json"),
        "client_name": "atmospheric-horizon",
        "client_uri": base,
        "redirect_uris": [format!("{base}/link/callback")],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "scope": SCOPE,
        "token_endpoint_auth_method": "none",
        "application_type": "web",
        "dpop_bound_access_tokens": true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_metadata_names_the_public_callback() {
        let metadata = client_metadata("https://links.example/");
        assert_eq!(
            metadata["client_id"],
            "https://links.example/oauth-client-metadata.json"
        );
        assert_eq!(
            metadata["redirect_uris"][0],
            "https://links.example/link/callback"
        );
        assert_eq!(metadata["application_type"], "web");
        assert_eq!(metadata["dpop_bound_access_tokens"], true);
    }
}
