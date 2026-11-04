//! Search, through pathfinder's `searchDesktop` (no Web API).

use descant_core::track::TrackInfo;
use librespot_core::{Error, session::Session};
use serde_json::{Value, json};

use crate::{
    library::{AlbumSummary, ArtistSummary, PlaylistSummary},
    net::{self, covers, items, text},
    pathfinder,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub tracks: Vec<TrackInfo>,
    pub artists: Vec<ArtistSummary>,
    pub albums: Vec<AlbumSummary>,
    pub playlists: Vec<PlaylistSummary>,
    /// Episodes come along in the same answer; it does not name their show.
    pub episodes: Vec<crate::podcasts::Episode>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
            && self.artists.is_empty()
            && self.albums.is_empty()
            && self.playlists.is_empty()
            && self.episodes.is_empty()
    }
}

/// Searches for `term`: up to `limit` of each kind.
pub async fn search(session: &Session, term: &str, limit: u32) -> Result<SearchResults, Error> {
    let data = pathfinder::query(
        session,
        &pathfinder::SEARCH_DESKTOP,
        json!({
            "searchTerm": term,
            "offset": 0,
            "limit": limit,
            "numberOfTopResults": 5,
            "includeAudiobooks": false,
        }),
    )
    .await?;
    Ok(parse(&data))
}

/// Reads a `searchDesktop` response's `data`. Anything missing is skipped,
/// never a reason to fail the whole search.
pub(crate) fn parse(data: &Value) -> SearchResults {
    let search = &data["search"];
    SearchResults {
        tracks: items(&search["tracks"])
            .filter_map(|i| net::track(&i["track"], "album"))
            .collect(),
        artists: items(&search["artists"]).filter_map(net::artist).collect(),
        albums: items(&search["albums"]).filter_map(album).collect(),
        playlists: items(&search["playlists"]).filter_map(playlist).collect(),
        episodes: items(&search["episodes"])
            .filter_map(|i| crate::podcasts::episode(&i["episode"]))
            .collect(),
    }
}

fn album(a: &Value) -> Option<AlbumSummary> {
    Some(AlbumSummary {
        uri: a["uri"]
            .as_str()
            .filter(|u| u.starts_with("spotify:album:"))?
            .to_string(),
        name: text(&a["name"]),
        artists: items(&a["artists"])
            .map(|x| text(&x["profile"]["name"]))
            .collect(),
        year: a["date"]["year"].as_i64().map(|y| y as i32),
        covers: covers(&a["coverArt"]["sources"]),
    })
}

fn playlist(p: &Value) -> Option<PlaylistSummary> {
    Some(PlaylistSummary {
        uri: p["uri"]
            .as_str()
            .filter(|u| u.starts_with("spotify:playlist:"))?
            .to_string(),
        name: text(&p["name"]),
        owner: text(&p["owner"]["name"]),
        covers: covers(&p["images"]["items"][0]["sources"]),
        description: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        json!({"search": {
            "tracks": {"items": [
                {"track": {
                    "uri": "spotify:track:t1", "name": "Airbag",
                    "artists": {"items": [{"profile": {"name": "Radiohead"}, "uri": "spotify:artist:a1"}]},
                    "album": {"name": "OK Computer", "uri": "spotify:album:b1",
                              "coverArt": {"sources": [
                                  {"url": "https://i.scdn.co/image/l", "width": 640, "height": 640},
                                  {"url": "https://i.scdn.co/image/s", "width": 64, "height": 64}]}},
                    "duration": {"totalMilliseconds": 284000},
                    "playability": {"playable": true}}},
                {"track": {"uri": "spotify:track:gone", "name": "Unplayable",
                           "playability": {"playable": false}}},
                {"track": {"name": "no uri"}}
            ]},
            "albums": {"items": [{"uri": "spotify:album:b1", "name": "OK Computer",
                "artists": {"items": [{"profile": {"name": "Radiohead"}}]},
                "coverArt": {"sources": [{"url": "https://i.scdn.co/image/c", "width": 300}]},
                "date": {"year": 1997}}]},
            "artists": {"items": [{"uri": "spotify:artist:a1", "profile": {"name": "Radiohead"},
                "visuals": {"avatarImage": {"sources": [{"url": "https://i.scdn.co/image/p", "width": 320}]}}}]},
            "episodes": {"items": [{"episode": {"uri": "spotify:episode:e1",
                "name": "A Landslide Victory for Democrats",
                "duration": {"totalMilliseconds": 2079973},
                "releaseDate": {"isoString": "2026-11-04T10:51:00Z"},
                "podcast": {"coverArt": {"sources": []}},
                "coverArt": {"sources": [{"url": "https://i.scdn.co/image/e", "width": 64}]}}}]},
            "playlists": {"items": [{"uri": "spotify:playlist:p1", "name": "This Is Radiohead",
                "owner": {"name": "Spotify"},
                "images": {"items": [{"sources": [{"url": "https://i.scdn.co/image/q", "width": null}]}]}}]}
        }})
    }

    #[test]
    fn tracks_come_with_artists_album_duration_and_covers() {
        let r = parse(&fixture());
        assert_eq!(
            r.tracks.len(),
            1,
            "unplayable and URI-less tracks are left out"
        );
        let t = &r.tracks[0];
        assert_eq!(t.uri, "spotify:track:t1");
        assert_eq!(t.title, "Airbag");
        assert_eq!(t.artists, vec!["Radiohead".to_string()]);
        assert_eq!(t.artist_uris, vec!["spotify:artist:a1".to_string()]);
        assert_eq!(t.album, "OK Computer");
        assert_eq!(t.duration_ms, 284_000);
        assert_eq!(
            t.covers.iter().map(|c| c.width).collect::<Vec<_>>(),
            vec![64, 640]
        );
    }

    #[test]
    fn episodes_come_along_without_their_show() {
        let r = parse(&fixture());
        assert_eq!(r.episodes.len(), 1);
        let e = &r.episodes[0];
        assert_eq!(e.name, "A Landslide Victory for Democrats");
        assert_eq!(e.released, Some((2026, 11, 4)));
        assert_eq!(e.show, "", "this answer does not name the show");
        assert!(!r.is_empty());
    }

    #[test]
    fn albums_artists_and_playlists() {
        let r = parse(&fixture());
        assert_eq!(r.albums[0].name, "OK Computer");
        assert_eq!(r.albums[0].year, Some(1997));
        assert_eq!(r.albums[0].artists, vec!["Radiohead".to_string()]);
        assert_eq!(r.artists[0].name, "Radiohead");
        assert_eq!(r.artists[0].covers[0].width, 320);
        let p = &r.playlists[0];
        assert_eq!(
            (p.name.as_str(), p.owner.as_str()),
            ("This Is Radiohead", "Spotify")
        );
        assert_eq!(
            p.covers[0].width, 300,
            "a missing width counts as the default size"
        );
    }

    #[test]
    fn an_empty_or_odd_response_is_no_results() {
        assert!(parse(&json!(null)).is_empty());
        assert!(parse(&json!({"search": {"tracks": {"items": "nope"}}})).is_empty());
    }
}
