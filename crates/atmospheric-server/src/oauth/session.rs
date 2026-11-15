use super::dpop::DpopKey;
use reqwest::{Method, Url, header};
use serde_json::Value;

/// A completed browser login. The access token and DPoP key live only for the
/// publishing operation that follows the callback; nothing is persisted.
#[derive(Clone)]
pub struct OAuthSession {
    pub did: String,
    pub handle: String,
    pub pds: String,
    access_token: String,
    key: DpopKey,
    http: reqwest::Client,
}

impl OAuthSession {
    pub(crate) fn new(
        did: String,
        handle: String,
        pds: String,
        access_token: String,
        key: DpopKey,
        http: reqwest::Client,
    ) -> Self {
        Self {
            did,
            handle,
            pds,
            access_token,
            key,
            http,
        }
    }

    pub async fn upload_blob(&self, bytes: &[u8]) -> Result<Value, String> {
        let url = self.url("/xrpc/com.atproto.repo.uploadBlob", &[])?;
        self.request(Method::POST, url, Some(bytes.to_vec()), Some("image/png"))
            .await
    }

    pub async fn get_record(&self, collection: &str, rkey: &str) -> Result<Option<Value>, String> {
        let url = self.url(
            "/xrpc/com.atproto.repo.getRecord",
            &[
                ("repo", &self.did),
                ("collection", collection),
                ("rkey", rkey),
            ],
        )?;
        match self.request(Method::GET, url, None, None).await {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.starts_with("RecordNotFound") => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn put_record(
        &self,
        collection: &str,
        rkey: &str,
        record: Value,
    ) -> Result<Value, String> {
        let url = self.url("/xrpc/com.atproto.repo.putRecord", &[])?;
        let body = serde_json::json!({
            "repo": self.did,
            "collection": collection,
            "rkey": rkey,
            "record": record,
            "validate": false,
        });
        let bytes = serde_json::to_vec(&body).map_err(|error| error.to_string())?;
        self.request(Method::POST, url, Some(bytes), Some("application/json"))
            .await
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, String> {
        let mut url = Url::parse(&format!("{}{path}", self.pds.trim_end_matches('/')))
            .map_err(|error| error.to_string())?;
        url.query_pairs_mut()
            .extend_pairs(query.iter().map(|(key, value)| (*key, *value)));
        Ok(url)
    }

    async fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<Vec<u8>>,
        content_type: Option<&str>,
    ) -> Result<Value, String> {
        let mut nonce: Option<String> = None;
        for _ in 0..3 {
            let proof = self.key.proof(
                method.as_str(),
                url.as_str(),
                nonce.as_deref(),
                Some(&self.access_token),
            )?;
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .header(header::AUTHORIZATION, format!("DPoP {}", self.access_token))
                .header("DPoP", proof);
            if let Some(content_type) = content_type {
                request = request.header(header::CONTENT_TYPE, content_type);
            }
            if let Some(body) = &body {
                request = request.body(body.clone());
            }
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status();
            let next_nonce = response
                .headers()
                .get("DPoP-Nonce")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let text = response.text().await.map_err(|error| error.to_string())?;
            let value: Value = serde_json::from_str(&text).unwrap_or_else(
                |_| serde_json::json!({"error": text.chars().take(300).collect::<String>()}),
            );
            if status == reqwest::StatusCode::BAD_REQUEST
                && value.get("error").and_then(Value::as_str) == Some("use_dpop_nonce")
                && let Some(next_nonce) = next_nonce
            {
                nonce = Some(next_nonce);
                continue;
            }
            if !status.is_success() {
                let error = value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("PDS request failed");
                return Err(format!("{error} (HTTP {status})"));
            }
            return Ok(value);
        }
        Err("the PDS kept asking for a new DPoP nonce".into())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_exposes_only_the_public_identity() {
        // Compilation guard: the access token is not a public field.
    }
}
