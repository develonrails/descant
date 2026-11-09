//! What every request to Spotify shares: one HTTP client, the session's
//! tokens, and a few ids and types.

use std::sync::LazyLock;

use descant_core::track::{Cover, TrackInfo};
use librespot_core::{Error, SpotifyId, session::Session};
use serde_json::Value;

use crate::library::ArtistSummary;

/// The collection API's protobuf content type.
pub(crate) const COLLECTION_TYPE: &str = "application/vnd.collection-v2.spotify.proto";

/// One client for the whole app: connections and TLS sessions are reused
/// instead of set up again for every cover, search and shelf.
pub fn http() -> &'static reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);
    &CLIENT
}

/// A request carrying the session's login5 bearer and client token, which
/// spclient and pathfinder want.
pub(crate) async fn authed(
    session: &Session,
    method: reqwest::Method,
    url: &str,
) -> Result<reqwest::RequestBuilder, Error> {
    let token = session.login5().auth_token().await?;
    let client_token = session.spclient().client_token().await?;
    Ok(http()
        .request(method, url)
        .bearer_auth(&token.access_token)
        .header("client-token", client_token))
}

/// The id of a `spotify:playlist:` URI, checked.
pub(crate) fn playlist_id(uri: &str) -> Result<&str, Error> {
    let id = uri
        .strip_prefix("spotify:playlist:")
        .ok_or_else(|| Error::invalid_argument("not a playlist URI"))?;
    SpotifyId::from_base62(id).map_err(|_| Error::invalid_argument("bad playlist id"))?;
    Ok(id)
}

// ---- pathfinder JSON, shared by search and home ---------------------------------

pub(crate) fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// Image sources, smallest first; a missing width counts as the default 300.
pub(crate) fn covers(sources: &Value) -> Vec<Cover> {
    let mut covers: Vec<Cover> = sources
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(Cover::new(
                s["url"].as_str()?,
                s["width"].as_u64().unwrap_or(300) as u32,
            ))
        })
        .collect();
    covers.sort_by_key(|c| c.width);
    covers
}

/// The items of a JSON list, without copying it.
pub(crate) fn items(value: &Value) -> impl Iterator<Item = &Value> {
    value["items"].as_array().into_iter().flatten()
}

/// A pathfinder track; its album sits under `album_key` (`album` in search,
/// `albumOfTrack` elsewhere). Unplayable tracks and ones without a URI are left out.
pub(crate) fn track(t: &Value, album_key: &str) -> Option<TrackInfo> {
    if t["playability"]["playable"].as_bool() == Some(false) {
        return None;
    }
    let album = &t[album_key];
    Some(TrackInfo {
        uri: t["uri"]
            .as_str()
            .filter(|u| u.starts_with("spotify:track:"))?
            .to_string(),
        title: text(&t["name"]),
        artists: items(&t["artists"])
            .map(|a| text(&a["profile"]["name"]))
            .collect(),
        artist_uris: items(&t["artists"])
            .filter_map(|a| a["uri"].as_str().map(String::from))
            .collect(),
        album: text(&album["name"]),
        album_uri: album["uri"]
            .as_str()
            .filter(|u| u.starts_with("spotify:album:"))
            .unwrap_or_default()
            .to_string(),
        duration_ms: t["duration"]["totalMilliseconds"].as_u64().unwrap_or(0) as u32,
        covers: covers(&album["coverArt"]["sources"]),
    })
}

pub(crate) fn artist(a: &Value) -> Option<ArtistSummary> {
    Some(ArtistSummary {
        uri: a["uri"]
            .as_str()
            .filter(|u| u.starts_with("spotify:artist:"))?
            .to_string(),
        name: text(&a["profile"]["name"]),
        covers: covers(&a["visuals"]["avatarImage"]["sources"]),
    })
}
