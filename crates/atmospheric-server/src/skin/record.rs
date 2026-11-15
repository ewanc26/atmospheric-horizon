use atmospheric::{
    records::{self, BytesB64, Endorsement, SkinModel, SkinRecord},
    signing::{self, SigningKey, SigningKeyFile},
};
use atrium_api::types::{BlobRef, string::Datetime};
use std::fmt;

/// The game's Ed25519 endorsement key, shared with the browser service so a
/// skin written there verifies exactly like one written by `/atmospheric skin`.
#[derive(Clone)]
pub struct SkinSigner {
    key: SigningKey,
    key_id: String,
}

impl fmt::Debug for SkinSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SkinSigner").field(&self.key_id).finish()
    }
}

impl SkinSigner {
    pub fn new(key: SigningKey) -> Self {
        let key_id = signing::key_id_of(&key.verifying_key());
        Self { key, key_id }
    }

    pub fn from_file(file: SigningKeyFile) -> Result<Self, String> {
        let key = signing::from_key_file(&file).map_err(|error| error.to_string())?;
        Ok(Self::new(key))
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Build the server endorsement over a new skin blob, preserving the
    /// existing cape and all other servers' endorsements. A malformed existing
    /// record is treated as absent, matching the M9 writer's tolerant upsert.
    pub fn build_record(
        &self,
        did: &str,
        skin: BlobRef,
        existing: Option<&serde_json::Value>,
        model: SkinModel,
    ) -> SkinRecord {
        let existing = existing
            .cloned()
            .and_then(|value| serde_json::from_value::<SkinRecord>(value).ok());
        let cape = existing.as_ref().and_then(|record| record.cape.clone());
        let mut endorsements = existing
            .map(|record| record.endorsements)
            .unwrap_or_default();
        endorsements.retain(|endorsement| endorsement.server != self.key_id);
        let mut record = SkinRecord {
            skin: Some(skin),
            cape,
            model,
            timestamp: Datetime::now(),
            endorsements,
        };
        let signature = signing::sign_b64(&self.key, &record.signable_payload(did, &self.key_id));
        record.endorsements.push(Endorsement {
            server: self.key_id.clone(),
            alg: signing::ALG_ED25519.into(),
            sig: BytesB64 { value: signature },
        });
        record
    }
}

/// The record key is the signing key id, matching the M9 writer and atpcraft.
pub fn rkey(key_id: &str) -> &str {
    key_id
}

/// The lexicon's skin collection, re-exported so the API module does not need
/// to know the atmospheric crate layout.
pub const COLLECTION: &str = records::SKIN_COLLECTION;

#[cfg(test)]
mod tests {
    use super::*;
    use atmospheric::records::{blob_cid, verify_endorsements};
    use std::collections::HashMap;

    const CID: &str = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";

    fn blob() -> BlobRef {
        serde_json::from_value(serde_json::json!({
            "$type": "blob",
            "ref": {"$link": CID},
            "mimeType": "image/png",
            "size": 1234,
        }))
        .unwrap()
    }

    #[test]
    fn builds_a_verifiable_record_and_preserves_foreign_endorsements() {
        let key = signing::generate();
        let signer = SkinSigner::new(key.clone());
        let existing = serde_json::json!({
            "skin": blob(),
            "model": "default",
            "timestamp": "2026-11-12T12:00:00.000Z",
            "endorsements": [
                {"server": "other", "alg": "ed25519", "sig": {"$bytes": "c2ln"}},
                {"server": signer.key_id(), "alg": "ed25519", "sig": {"$bytes": "c2ln"}}
            ]
        });
        let record = signer.build_record("did:plc:alice", blob(), Some(&existing), SkinModel::Slim);
        assert_eq!(record.model, SkinModel::Slim);
        assert_eq!(record.endorsements.len(), 2);
        assert!(record.endorsements.iter().any(|e| e.server == "other"));
        assert_eq!(
            record
                .endorsements
                .iter()
                .filter(|e| e.server == signer.key_id())
                .count(),
            1
        );
        let keys = HashMap::from([(signer.key_id().to_string(), key.verifying_key())]);
        assert!(matches!(
            verify_endorsements(
                &serde_json::to_value(&record.endorsements).unwrap(),
                &keys,
                |server| {
                    (server == signer.key_id())
                        .then(|| record.signable_payload("did:plc:alice", server))
                }
            ),
            atmospheric::records::EndorsementVerdict::Verified { .. }
        ));
        assert_eq!(
            blob_cid(record.skin.as_ref().unwrap()).as_deref(),
            Some(CID)
        );
    }

    #[test]
    fn a_malformed_existing_record_does_not_block_a_new_one() {
        let signer = SkinSigner::new(signing::generate());
        let record = signer.build_record(
            "did:plc:alice",
            blob(),
            Some(&serde_json::json!({"model": "wide"})),
            SkinModel::Default,
        );
        assert!(record.cape.is_none());
        assert_eq!(record.endorsements.len(), 1);
    }
}
