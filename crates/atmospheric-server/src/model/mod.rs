//! Wire types and the validation/normalisation rules shared by the API and
//! the store. Everything here is pure: no I/O, no HTTP.

mod browser;
mod health;
mod labeler;
mod link;
mod pairing;
mod time;
mod validate;

pub use browser::{BrowserPairingInput, BrowserPairingResponse};
pub use health::Health;
pub use labeler::EmitLabelInput;
pub use link::{Link, LinkInput, LinksResponse};
pub(crate) use pairing::new_pairing_code;
pub use pairing::{
    ClaimInput, DEFAULT_PAIRING_TTL_SECS, MAX_PAIRING_TTL_SECS, MIN_PAIRING_TTL_SECS, PairingInput,
    PairingResponse, normalize_code, pairing_ttl,
};
pub use time::unix_now;
pub use validate::{normalize_did, normalize_handle, normalize_player_name, normalize_uuid};
