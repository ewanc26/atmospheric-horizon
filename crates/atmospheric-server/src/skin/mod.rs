//! Browser skin upload (M64): one-time links, PNG validation, and signed
//! `at.atpcraft.player.skin` writes performed through the player's browser
//! OAuth session. In-game application is not part of this module because the
//! pinned Horizon native API has no skin/texture setter.

mod png;
mod record;
mod state;

pub use png::validate_skin;
pub use record::{COLLECTION, SkinSigner, rkey};
pub use state::{PendingSkin, PendingSkinAuth, SKIN_UPLOAD_TTL_SECS, SkinUploads};
