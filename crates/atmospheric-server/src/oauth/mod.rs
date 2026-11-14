//! The public-client AT Protocol OAuth flow used by the browser link page.
//!
//! This mirrors the spec (PAR, PKCE S256, DPoP-bound tokens). Unlike the mod's
//! loopback login, the redirect is a public HTTPS URL, so the service publishes
//! client metadata at `/oauth-client-metadata.json` and uses that URL as the
//! client id.

mod client;
mod dpop;
mod encoding;
mod resolver;
#[cfg(test)]
mod tests;

pub use client::{Identity, OAuthClient, PendingAuthorization, SCOPE, Started, client_metadata};
pub use dpop::DpopKey;
pub(crate) use encoding::random_token;
pub use resolver::Resolver;
