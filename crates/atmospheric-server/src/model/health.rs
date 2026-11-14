use serde::Serialize;

/// Unauthenticated service health and collection counts.
#[derive(Debug, Serialize)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
    pub links: usize,
    pub pending_pairings: usize,
}
