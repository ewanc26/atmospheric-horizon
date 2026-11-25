//! Pluggable Proof-of-Handle verification for operator grants (#189).
//!
//! Production uses the real public-network chain from #185. Tests inject a
//! verifier so the HTTP surface can be exercised without contacting a PDS.

use atmospheric::{
    oauth,
    proof::{Challenge, HandleProof, verify_handle},
};
use std::{future::Future, pin::Pin, sync::Arc};

/// Boxed future returned by a verifier.
pub type VerifyFuture = Pin<Box<dyn Future<Output = Result<HandleProof, String>> + Send>>;

/// Verification hook. It receives the allowlisted DID and the issued challenge
/// and must return a verified proof bound to that DID.
pub type VerifyFn = Arc<dyn Fn(String, Challenge) -> VerifyFuture + Send + Sync>;

/// The production verifier: the same PLC-anchored commit chain as #185.
pub fn public_verify(http: reqwest::Client, resolver: oauth::Resolver) -> VerifyFn {
    Arc::new(move |did, challenge| {
        let http = http.clone();
        let resolver = resolver.clone();
        Box::pin(async move {
            verify_handle(&http, &resolver, &did, &challenge)
                .await
                .map_err(|error| error.to_string())
        })
    })
}
