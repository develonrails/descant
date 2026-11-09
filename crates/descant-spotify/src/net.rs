//! What every request to Spotify shares: one HTTP client, the session's
//! tokens, and a few ids and types.

use std::sync::LazyLock;

use descant_core::track::{Cover, TrackInfo};
use librespot_core::{Error, SpotifyId, session::Session};

use crate::{
    lenient::{Items, shape},
    library::ArtistSummary,
};

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

// ---- pathfinder JSON, shared by search, home and podcasts ------------------------

shape! {
    /// `{"name": …}`: a profile, an owner, a publisher.
    pub(crate) struct Named {
        name: Option<String>,
    }
}

shape! {
    pub(crate) struct Source {
        url: Option<String>,
        width: Option<u64>,
    }
}

shape! {
    /// `coverArt`, `avatarImage`, one of a playlist's `images`.
    pub(crate) struct Image {
        sources: Vec<Source>,
    }
}

shape! {
    pub(crate) struct Duration {
        total_milliseconds: Option<u64>,
    }
}

shape! {
    pub(crate) struct Playability {
        playable: Option<bool>,
    }
}

shape! {
    pub(crate) struct TrackArtist {
        uri: Option<String>,
        profile: Named,
    }
}

shape! {
    pub(crate) struct TrackAlbum {
        uri: Option<String>,
        name: Option<String>,
        cover_art: Image,
    }
}

shape! {
    /// A pathfinder track; search names its album `album`, the rest
    /// `albumOfTrack`.
    pub(crate) struct Track {
        uri: Option<String>,
        name: Option<String>,
        artists: Items<TrackArtist>,
        album: TrackAlbum,
        album_of_track: TrackAlbum,
        duration: Duration,
        playability: Playability,
    }
}

shape! {
    pub(crate) struct Visuals {
        avatar_image: Image,
    }
}

shape! {
    pub(crate) struct Artist {
        uri: Option<String>,
        profile: Named,
        visuals: Visuals,
    }
}

/// `uri` if it starts with `prefix` (`spotify:track:` and the like).
pub(crate) fn with_prefix(uri: Option<String>, prefix: &str) -> Option<String> {
    uri.filter(|u| u.starts_with(prefix))
}

/// Image sources, smallest first; a missing width counts as the default 300.
pub(crate) fn covers(image: Image) -> Vec<Cover> {
    let mut covers: Vec<Cover> = image
        .sources
        .into_iter()
        .filter_map(|s| {
            Some(Cover {
                url: s.url?,
                width: s.width.unwrap_or(300) as u32,
            })
        })
        .collect();
    covers.sort_by_key(|c| c.width);
    covers
}

/// Which of a track's albums to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AlbumKey {
    /// `album`, as search names it.
    Album,
    /// `albumOfTrack`, as everything else names it.
    AlbumOfTrack,
}

/// A pathfinder track. Unplayable tracks and ones without a URI are left out.
pub(crate) fn track(t: Track, album_key: AlbumKey) -> Option<TrackInfo> {
    if t.playability.playable == Some(false) {
        return None;
    }
    let uri = with_prefix(t.uri, "spotify:track:")?;
    let album = match album_key {
        AlbumKey::Album => t.album,
        AlbumKey::AlbumOfTrack => t.album_of_track,
    };
    let mut artists = Vec::with_capacity(t.artists.items.len());
    let mut artist_uris = Vec::with_capacity(t.artists.items.len());
    for a in t.artists.items {
        artists.push(a.profile.name.unwrap_or_default());
        artist_uris.extend(a.uri);
    }
    Some(TrackInfo {
        uri,
        title: t.name.unwrap_or_default(),
        artists,
        artist_uris,
        album: album.name.unwrap_or_default(),
        album_uri: with_prefix(album.uri, "spotify:album:").unwrap_or_default(),
        duration_ms: t.duration.total_milliseconds.unwrap_or(0) as u32,
        covers: covers(album.cover_art),
    })
}

pub(crate) fn artist(a: Artist) -> Option<ArtistSummary> {
    Some(ArtistSummary {
        uri: with_prefix(a.uri, "spotify:artist:")?,
        name: a.profile.name.unwrap_or_default(),
        covers: covers(a.visuals.avatar_image),
    })
}
