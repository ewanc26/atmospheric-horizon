//! Server-side player identity linking for atmospheric-horizon.
//!
//! This crate is deliberately separate from the game worker: it is an optional
//! HTTP service that operators run on the Minecraft server. It maps canonical
//! Minecraft player UUIDs to ATProto DIDs and issues one-time pairing codes so a
//! player can claim a link without being handed the admin token.

pub mod api;
pub mod browser;
pub mod error;
pub mod labeler;
pub mod model;
pub mod oauth;
mod page;
pub mod store;

pub use api::{ServerConfig, app};
