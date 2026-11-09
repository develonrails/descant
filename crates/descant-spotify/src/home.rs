//! What Home shows, all through the session (no Web API):
//!
//! - Made for You: pathfinder `homeSection` (spike S4)
//! - Your top artists and songs: pathfinder `userTopContent` (as measured by
//!   ralph/Spotifly, MIT)
//! - Recently played: spclient `recently-played/v3`, which lists bare context
//!   URIs; they are described here (albums and artists in one batch each,
//!   playlists one by one).
//!
//! Spotify does not record what librespot plays, so Recently played reflects
//! the user's other devices.

use std::collections::HashMap;

use descant_core::track::TrackInfo;
use librespot_core::{Error, session::Session};
use serde_json::{Value, json};

use crate::{
    library::{self, AlbumSummary, ArtistSummary, PlaylistSummary},
    net::{self, covers, items, text},
    pathfinder,
};

/// The Made for You section of Home.
const MADE_FOR_YOU: &str = "spotify:section:0JQ5DAUnp4wcj0bCb3wh3S";

/// Something on a Home shelf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Playlist(PlaylistSummary),
    Album(AlbumSummary),
    Artist(ArtistSummary),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Top {
    pub artists: Vec<ArtistSummary>,
    pub tracks: Vec<TrackInfo>,
}

pub async fn made_for_you(session: &Session) -> Result<Vec<PlaylistSummary>, Error> {
    let data = pathfinder::query(
        session,
        &pathfinder::HOME_SECTION,
        json!({
            "uri": MADE_FOR_YOU,
            "country": session.country(),
            "timeZone": "Europe/Amsterdam",
            "sp_t": "",
            "sectionItemsLimit": 20,
            "sectionItemsOffset": 0,
        }),
    )
    .await?;
    Ok(parse_section(&data))
}

pub async fn top(session: &Session, limit: u32) -> Result<Top, Error> {
    // Songs over the last weeks; artists over months, which the short range
    // often leaves nearly empty.
    let range =
        |time: &str| json!({"offset": 0, "limit": limit, "sortBy": "AFFINITY", "timeRange": time});
    let data = pathfinder::query(
        session,
        &pathfinder::USER_TOP_CONTENT,
        json!({
            "includeTopArtists": true,
            "topArtistsInput": range("MID_TERM"),
            "includeTopTracks": true,
            "topTracksInput": range("SHORT_TERM"),
        }),
    )
    .await?;
    Ok(parse_top(&data))
}

pub async fn recently_played(session: &Session, limit: usize) -> Result<Vec<Item>, Error> {
    let endpoint = format!(
        "/recently-played/v3/user/{}/recently-played?format=json&offset=0&limit={limit}&filter=default",
        session.username()
    );
    let bytes = session
        .spclient()
        .request(&http::Method::GET, &endpoint, None, None)
        .await?;
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let uris = recent_uris(&value);
    let of_kind = |kind: &str| -> Vec<String> {
        uris.iter()
            .filter(|u| u.starts_with(kind))
            .cloned()
            .collect()
    };
    let (album_uris, artist_uris) = (of_kind("spotify:album:"), of_kind("spotify:artist:"));

    // Albums, artists and each playlist's header, all at once.
    let mut headers = tokio::task::JoinSet::new();
    for uri in of_kind("spotify:playlist:") {
        let session = session.clone();
        headers.spawn(async move {
            let page = library::playlist(&session, &uri, 0, 0).await.ok()?;
            Some(PlaylistSummary {
                uri,
                name: page.name,
                owner: page.owner,
                covers: page.covers,
                description: String::new(),
            })
        });
    }
    let (albums, artists) = tokio::join!(
        library::albums(session, &album_uris),
        library::artists(session, &artist_uris)
    );
    let albums: HashMap<String, AlbumSummary> =
        albums?.into_iter().map(|a| (a.uri.clone(), a)).collect();
    let artists: HashMap<String, ArtistSummary> =
        artists?.into_iter().map(|a| (a.uri.clone(), a)).collect();
    let mut playlists = HashMap::new();
    while let Some(done) = headers.join_next().await {
        if let Ok(Some(summary)) = done {
            playlists.insert(summary.uri.clone(), summary);
        }
    }
    Ok(uris
        .iter()
        .filter_map(|uri| {
            if let Some(a) = albums.get(uri) {
                Some(Item::Album(a.clone()))
            } else if let Some(a) = artists.get(uri) {
                Some(Item::Artist(a.clone()))
            } else {
                playlists.remove(uri).map(Item::Playlist)
            }
        })
        .collect())
}

// ---- parsing (pure) --------------------------------------------------------------

/// Playlists in a `homeSection` answer.
pub(crate) fn parse_section(data: &Value) -> Vec<PlaylistSummary> {
    data["homeSections"]["sections"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|section| items(&section["sectionItems"]))
        .filter_map(|item| {
            let d = &item["content"]["data"];
            let uri = d["uri"]
                .as_str()
                .filter(|u| u.starts_with("spotify:playlist:"))?;
            Some(PlaylistSummary {
                uri: uri.to_string(),
                name: text(&d["name"]),
                owner: text(&d["ownerV2"]["data"]["name"]),
                covers: covers(&d["images"]["items"][0]["sources"]),
                description: strip_tags(&text(&d["description"])),
            })
        })
        .collect()
}

/// Top artists and tracks in a `userTopContent` answer.
pub(crate) fn parse_top(data: &Value) -> Top {
    let profile = &data["me"]["profile"];
    Top {
        artists: items(&profile["topArtists"])
            .filter_map(|i| net::artist(&i["data"]))
            .collect(),
        tracks: items(&profile["topTracks"])
            .filter_map(|i| net::track(&i["data"], "albumOfTrack"))
            .collect(),
    }
}

/// The context URIs in a `recently-played/v3` answer, newest first; podcasts
/// are out of scope.
pub(crate) fn recent_uris(value: &Value) -> Vec<String> {
    value["playContexts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["uri"].as_str())
        .filter(|u| {
            ["spotify:playlist:", "spotify:album:", "spotify:artist:"]
                .iter()
                .any(|p| u.starts_with(p))
        })
        .map(String::from)
        .collect()
}

/// Spotify's descriptions may hold links (`<a href=…>Moby</a>`).
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&#x27;", "'")
        .replace("&quot;", "\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn made_for_you_reads_playlists_with_descriptions() {
        let data = json!({"homeSections": {"sections": [{"sectionItems": {"items": [
            {"content": {"data": {"uri": "spotify:playlist:dw", "name": "Discover Weekly",
                "description": "Your weekly mixtape of <a href=\"x\">fresh</a> music",
                "ownerV2": {"data": {"name": "Spotify"}},
                "images": {"items": [{"sources": [{"url": "https://i.scdn.co/image/d", "width": 300}]}]}}}},
            {"content": {"data": {"uri": "spotify:show:pod", "name": "a podcast"}}}
        ]}}]}});
        let got = parse_section(&data);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "Discover Weekly");
        assert_eq!(got[0].description, "Your weekly mixtape of fresh music");
        assert_eq!(got[0].covers[0].url(), "https://i.scdn.co/image/d");
    }

    #[test]
    fn top_content_reads_artists_and_tracks() {
        let data = json!({"me": {"profile": {
            "topArtists": {"items": [{"data": {"uri": "spotify:artist:moby", "profile": {"name": "Moby"},
                "visuals": {"avatarImage": {"sources": [{"url": "u", "width": 320}]}}}}]},
            "topTracks": {"items": [{"data": {"uri": "spotify:track:five", "name": "5 Years Time",
                "artists": {"items": [{"profile": {"name": "Noah And The Whale"}, "uri": "spotify:artist:n"}]},
                "albumOfTrack": {"name": "Peaceful, The World Lays Me Down", "uri": "spotify:album:p",
                    "coverArt": {"sources": [{"url": "c", "width": 64}]}},
                "duration": {"totalMilliseconds": 214000}}}]}
        }}});
        let top = parse_top(&data);
        assert_eq!(top.artists[0].name, "Moby");
        let t = &top.tracks[0];
        assert_eq!(
            (t.title.as_str(), t.album_uri.as_str()),
            ("5 Years Time", "spotify:album:p")
        );
        assert_eq!(t.artist_line(), "Noah And The Whale");
        assert_eq!(t.duration_ms, 214_000);
    }

    #[test]
    fn recently_played_keeps_music_contexts_in_order() {
        let value = json!({"playContexts": [
            {"uri": "spotify:artist:a"}, {"uri": "spotify:show:podcast"},
            {"uri": "spotify:playlist:p"}, {"uri": "spotify:album:b"}
        ]});
        assert_eq!(
            recent_uris(&value),
            vec!["spotify:artist:a", "spotify:playlist:p", "spotify:album:b"]
        );
        assert!(recent_uris(&json!(null)).is_empty());
    }
}
