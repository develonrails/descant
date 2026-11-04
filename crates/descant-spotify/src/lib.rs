//! Descant's Spotify side, all through the playback session (no Web API):
//!
//! - [`pairing`]: log in by scanning a QR code with the phone that has
//!   Spotify on it; [`store`] keeps the credentials in the keyring
//! - [`library`], [`home`], [`search`]: reading playlists, the collection,
//!   albums, artists, Home's shelves and search results
//! - [`edit`]: liking tracks and editing playlists
//! - [`pathfinder`]: Spotify's internal GraphQL, its query hashes in one table

mod collection;
mod net;

pub use net::http;
pub mod edit;
pub mod home;
pub mod library;
pub mod pairing;
pub mod pathfinder;
pub mod profile;
pub mod search;
pub mod store;
