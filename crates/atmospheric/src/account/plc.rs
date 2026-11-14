//! Publishing the labeler identity in the server account's PLC DID document
//! (M60, part of #112).
//!
//! The M59 `app.bsky.labeler.service` declaration describes the vocabulary,
//! but clients discover a labeler through the account's DID document:
//! `#atproto_label` carries the K-256 key that verifies each signed label and
//! `#atproto_labeler` names the public HTTP endpoint serving
//! `com.atproto.label.*`. Both entries live in the `did:plc` operation, which
//! only the server account's OAuth session can sign and submit.
//!
//! The labeler's public half (service DID, signing `did:key` and endpoint) is
//! served by `atmospheric-server`'s `GET /v1/labeler/identity`. This module
//! reads that, reads the account's current operation from the public PLC
//! directory, merges the two labeler entries into it while preserving every
//! existing entry, and drives `com.atproto.identity.signPlcOperation` plus
//! `submitPlcOperation` on the account's PDS. Wire shapes come from the public
//! lexicons and PLC specification only; no atpcraft code is copied.

use crate::{
    error::{AtmosphericError, Result},
    session::Agent,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The public PLC directory. `did:web` accounts cannot be updated here.
pub const PLC_DIRECTORY: &str = "https://plc.directory";

/// The identity path served by `atmospheric-server` when its labeler runs.
pub const LABELER_IDENTITY_PATH: &str = "/v1/labeler/identity";

/// The verification-method fragment that publishes the label signing key.
pub const LABEL_KEY_FRAGMENT: &str = "atproto_label";

/// The service fragment that publishes the labeler endpoint.
pub const LABELER_SERVICE_FRAGMENT: &str = "atproto_labeler";

/// The DID-document service type clients expect for a labeler.
pub const LABELER_SERVICE_TYPE: &str = "AtprotoLabeler";

/// The labeler's public identity, as the labeler service reports it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelerService {
    /// The service DID labels are signed with (the server account's `did:plc`).
    pub did: String,
    /// The compressed K-256 public key as a multibase `did:key`.
    pub signing_key: String,
    /// The public base URL of the labeler HTTP service, when configured.
    #[serde(default)]
    pub endpoint: Option<String>,
}

/// One `did:plc` operation service entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlcService {
    pub r#type: String,
    pub endpoint: String,
}

/// The fields of a `did:plc` operation this milestone reads and rewrites.
///
/// `token` is the one-time token the PDS emails for an identity operation; the
/// PLC directory never returns it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcOperation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rotation_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_known_as: Vec<String>,
    #[serde(default)]
    pub verification_methods: BTreeMap<String, String>,
    #[serde(default)]
    pub services: BTreeMap<String, PlcService>,
}

impl PlcOperation {
    /// Read the operation fields out of a PLC directory `/data` response,
    /// dropping its unrelated metadata (`did`, `signer`, `createdAt`, ...).
    pub fn from_plc_data(value: &serde_json::Value) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PlcData {
            #[serde(default)]
            rotation_keys: Vec<String>,
            #[serde(default)]
            also_known_as: Vec<String>,
            #[serde(default)]
            verification_methods: BTreeMap<String, String>,
            #[serde(default)]
            services: BTreeMap<String, PlcService>,
        }
        let data: PlcData = serde_json::from_value(value.clone())
            .map_err(|error| AtmosphericError::Message(format!("invalid PLC data: {error}")))?;
        Ok(Self {
            token: None,
            rotation_keys: data.rotation_keys,
            also_known_as: data.also_known_as,
            verification_methods: data.verification_methods,
            services: data.services,
        })
    }

    /// Apply the current labeler key and endpoint, overwriting stale values
    /// but leaving every other entry untouched. Re-applying unconditionally
    /// refreshes the DID document after a key rotation or a URL change.
    pub fn with_labeler(
        mut self,
        signing_key: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        self.verification_methods
            .insert(LABEL_KEY_FRAGMENT.into(), signing_key.into());
        self.services.insert(
            LABELER_SERVICE_FRAGMENT.into(),
            PlcService {
                r#type: LABELER_SERVICE_TYPE.into(),
                endpoint: endpoint.into(),
            },
        );
        self
    }

    /// Whether the published key and service are both present.
    pub fn has_labeler_entries(&self) -> (bool, bool) {
        (
            self.verification_methods.contains_key(LABEL_KEY_FRAGMENT),
            self.services.contains_key(LABELER_SERVICE_FRAGMENT),
        )
    }

    /// Whether the published entries already match the announced identity.
    pub fn matches_labeler(&self, service: &LabelerService) -> (bool, bool) {
        let key = self
            .verification_methods
            .get(LABEL_KEY_FRAGMENT)
            .is_some_and(|key| key == &service.signing_key);
        let endpoint = service.endpoint.as_deref().is_some_and(|endpoint| {
            self.services
                .get(LABELER_SERVICE_FRAGMENT)
                .is_some_and(|entry| entry.endpoint == endpoint)
        });
        (key, endpoint)
    }

    /// The `com.atproto.identity.signPlcOperation` request body. Empty arrays
    /// are omitted so an operation never clears an entry it did not set.
    pub fn sign_input(&self) -> serde_json::Value {
        let mut object = serde_json::Map::new();
        if let Some(token) = &self.token {
            object.insert("token".into(), token.clone().into());
        }
        if !self.rotation_keys.is_empty() {
            object.insert("rotationKeys".into(), self.rotation_keys.clone().into());
        }
        if !self.also_known_as.is_empty() {
            object.insert("alsoKnownAs".into(), self.also_known_as.clone().into());
        }
        object.insert(
            "verificationMethods".into(),
            serde_json::to_value(&self.verification_methods).expect("a map serialises"),
        );
        object.insert(
            "services".into(),
            serde_json::to_value(&self.services).expect("a map serialises"),
        );
        serde_json::Value::Object(object)
    }
}

/// Validate a base URL before any request is made.
fn service_url(base: &str, path: &str) -> Result<String> {
    let base = base.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(AtmosphericError::Message(
            "server_api_url must start with http:// or https://".into(),
        ));
    }
    Ok(format!("{base}{path}"))
}

/// Read the labeler's public identity from the configured identity service.
pub async fn fetch_labeler_service(http: &reqwest::Client, base: &str) -> Result<LabelerService> {
    let url = service_url(base, LABELER_IDENTITY_PATH)?;
    let response = http
        .get(url)
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    let status = response.status();
    let text = response.text().await.map_err(AtmosphericError::protocol)?;
    if !status.is_success() {
        return Err(AtmosphericError::Message(format!(
            "labeler identity service: {}",
            service_error(status, &text)
        )));
    }
    let service: LabelerService = serde_json::from_str(&text)?;
    validate_labeler_service(&service)?;
    Ok(service)
}

/// Reject an identity the PLC update cannot publish.
pub fn validate_labeler_service(service: &LabelerService) -> Result<()> {
    if !service.did.starts_with("did:") || service.did.chars().any(char::is_whitespace) {
        return Err(AtmosphericError::Message(
            "the labeler service returned an invalid DID".into(),
        ));
    }
    if !service.signing_key.starts_with("did:key:z")
        || service.signing_key.chars().any(char::is_whitespace)
    {
        return Err(AtmosphericError::Message(
            "the labeler service returned an invalid signing key".into(),
        ));
    }
    if let Some(endpoint) = &service.endpoint
        && !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
    {
        return Err(AtmosphericError::Message(
            "the labeler service returned an invalid public URL".into(),
        ));
    }
    Ok(())
}

/// Fail early for a `did:web` account, whose service entries are published in
/// its own document rather than through PLC.
pub fn ensure_plc_did(did: &str) -> Result<()> {
    if did.starts_with("did:plc:") {
        Ok(())
    } else {
        Err(AtmosphericError::Message(format!(
            "{did} is not a did:plc account; PLC identity updates do not apply"
        )))
    }
}

/// Fetch an account's signed operation data from the public PLC directory.
pub async fn fetch_plc_data(
    http: &reqwest::Client,
    directory: &str,
    did: &str,
) -> Result<serde_json::Value> {
    ensure_plc_did(did)?;
    if did.chars().any(char::is_whitespace) {
        return Err(AtmosphericError::Message("invalid DID".into()));
    }
    let url = service_url(directory, &format!("/{did}/data"))?;
    let response = http
        .get(url)
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    let status = response.status();
    let text = response.text().await.map_err(AtmosphericError::protocol)?;
    if !status.is_success() {
        return Err(AtmosphericError::Message(format!(
            "PLC directory: {}",
            service_error(status, &text)
        )));
    }
    serde_json::from_str(&text)
        .map_err(|error| AtmosphericError::Message(format!("invalid PLC data: {error}")))
}

/// Ask the PDS to email a one-time PLC operation token for the logged-in
/// account (`com.atproto.identity.requestPlcOperationSignature`).
pub async fn request_signature(agent: &Agent) -> Result<()> {
    agent
        .api
        .com
        .atproto
        .identity
        .request_plc_operation_signature()
        .await
        .map_err(AtmosphericError::protocol)?;
    Ok(())
}

/// Sign the operation with the account's PDS
/// (`com.atproto.identity.signPlcOperation`) and submit the signed operation
/// to PLC (`com.atproto.identity.submitPlcOperation`).
pub async fn sign_and_submit(agent: &Agent, operation: &PlcOperation) -> Result<()> {
    let input = serde_json::from_value(operation.sign_input())
        .map_err(|error| AtmosphericError::Message(format!("invalid PLC operation: {error}")))?;
    let signed = agent
        .api
        .com
        .atproto
        .identity
        .sign_plc_operation(input)
        .await
        .map_err(AtmosphericError::protocol)?;
    let submit_input = serde_json::from_value(serde_json::json!({
        "operation": signed.data.operation,
    }))
    .map_err(|error| AtmosphericError::Message(format!("invalid signed PLC operation: {error}")))?;
    agent
        .api
        .com
        .atproto
        .identity
        .submit_plc_operation(submit_input)
        .await
        .map_err(AtmosphericError::protocol)?;
    Ok(())
}

fn service_error(status: reqwest::StatusCode, text: &str) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: Option<String>,
        message: Option<String>,
    }
    serde_json::from_str::<ErrorBody>(text)
        .ok()
        .and_then(|body| body.message.or(body.error))
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| format!("HTTP {status}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current() -> serde_json::Value {
        serde_json::json!({
            "did": "did:plc:server",
            "signer": "did:key:zQ3shsigner",
            "createdAt": "2026-01-01T00:00:00.000Z",
            "rotationKeys": ["did:key:zQ3shrotation"],
            "alsoKnownAs": ["at://server.example"],
            "verificationMethods": {
                "atproto_pds": "did:key:zQ3shpds"
            },
            "services": {
                "atproto_pds": {
                    "type": "AtprotoPersonalDataServer",
                    "endpoint": "https://pds.example"
                }
            }
        })
    }

    fn service() -> LabelerService {
        LabelerService {
            did: "did:plc:server".into(),
            signing_key: "did:key:zQ3shlabel".into(),
            endpoint: Some("https://labels.example".into()),
        }
    }

    #[test]
    fn the_labeler_entries_are_added_without_touching_existing_entries() {
        let operation = PlcOperation::from_plc_data(&current()).unwrap();
        assert_eq!(operation.rotation_keys, ["did:key:zQ3shrotation"]);
        assert_eq!(operation.also_known_as, ["at://server.example"]);
        assert_eq!(operation.has_labeler_entries(), (false, false));

        let merged = operation.with_labeler(
            service().signing_key.clone(),
            service().endpoint.clone().unwrap(),
        );
        assert_eq!(merged.has_labeler_entries(), (true, true));
        assert_eq!(merged.matches_labeler(&service()), (true, true));
        assert_eq!(
            merged
                .verification_methods
                .get("atproto_pds")
                .map(String::as_str),
            Some("did:key:zQ3shpds")
        );
        assert_eq!(
            merged
                .services
                .get("atproto_pds")
                .map(|s| s.endpoint.as_str()),
            Some("https://pds.example")
        );

        let input = merged.sign_input();
        assert_eq!(input["rotationKeys"][0], "did:key:zQ3shrotation");
        assert_eq!(input["alsoKnownAs"][0], "at://server.example");
        assert_eq!(
            input["verificationMethods"]["atproto_label"],
            "did:key:zQ3shlabel"
        );
        assert_eq!(
            input["services"]["atproto_labeler"]["type"],
            "AtprotoLabeler"
        );
        assert_eq!(
            input["services"]["atproto_labeler"]["endpoint"],
            "https://labels.example"
        );
        assert!(input.get("token").is_none());
    }

    #[test]
    fn a_stale_labeler_entry_is_overwritten_and_a_token_is_carried() {
        let mut operation = PlcOperation::from_plc_data(&current()).unwrap();
        operation
            .verification_methods
            .insert(LABEL_KEY_FRAGMENT.into(), "did:key:zQ3shold".into());
        operation.services.insert(
            LABELER_SERVICE_FRAGMENT.into(),
            PlcService {
                r#type: LABELER_SERVICE_TYPE.into(),
                endpoint: "https://old.example".into(),
            },
        );
        assert_eq!(operation.matches_labeler(&service()), (false, false));

        operation.token = Some("email-token".into());
        let merged = operation.with_labeler(
            service().signing_key.clone(),
            service().endpoint.clone().unwrap(),
        );
        assert_eq!(merged.matches_labeler(&service()), (true, true));
        assert_eq!(merged.sign_input()["token"], "email-token");
    }

    #[test]
    fn empty_arrays_are_omitted_so_an_operation_never_clears_them() {
        let operation = PlcOperation::default().with_labeler(
            service().signing_key.clone(),
            service().endpoint.clone().unwrap(),
        );
        let input = operation.sign_input();
        assert!(input.get("rotationKeys").is_none());
        assert!(input.get("alsoKnownAs").is_none());
        assert!(input.get("verificationMethods").is_some());
        assert!(input.get("services").is_some());
    }

    #[test]
    fn only_did_plc_accounts_can_be_updated() {
        assert!(ensure_plc_did("did:plc:server").is_ok());
        let error = ensure_plc_did("did:web:server.example").unwrap_err();
        assert!(error.to_string().contains("did:plc"));
    }

    #[test]
    fn the_identity_response_is_validated() {
        let service: LabelerService = serde_json::from_value(serde_json::json!({
            "did": "did:plc:server",
            "signingKey": "did:key:zQ3shlabel",
            "endpoint": "https://labels.example"
        }))
        .unwrap();
        validate_labeler_service(&service).unwrap();

        let mut missing_key = service.clone();
        missing_key.signing_key.clear();
        assert!(validate_labeler_service(&missing_key).is_err());

        let mut bad_endpoint = service.clone();
        bad_endpoint.endpoint = Some("labels.example".into());
        assert!(validate_labeler_service(&bad_endpoint).is_err());

        let mut bad_did = service;
        bad_did.did = "server".into();
        assert!(validate_labeler_service(&bad_did).is_err());
    }

    #[test]
    fn the_operation_fits_the_generated_lexicon_requests() {
        let operation = PlcOperation::from_plc_data(&current())
            .unwrap()
            .with_labeler("did:key:zQ3shlabel", "https://labels.example");
        let _: atrium_api::com::atproto::identity::sign_plc_operation::InputData =
            serde_json::from_value(operation.sign_input()).unwrap();
        let _: atrium_api::com::atproto::identity::submit_plc_operation::InputData =
            serde_json::from_value(serde_json::json!({
                "operation": {"type": "plc_operation"}
            }))
            .unwrap();
    }

    #[test]
    fn an_endpoint_without_a_configured_url_never_matches() {
        let operation = PlcOperation::from_plc_data(&current())
            .unwrap()
            .with_labeler("did:key:zQ3shlabel", "https://labels.example");
        let mut no_endpoint = service();
        no_endpoint.endpoint = None;
        assert_eq!(operation.matches_labeler(&no_endpoint), (true, false));
    }
}
