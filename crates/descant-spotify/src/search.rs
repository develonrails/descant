//! Search, through pathfinder's `searchDesktop` (no Web API).

use descant_core::track::TrackInfo;
use librespot_core::{Error, session::Session};
use serde_json::json;

use crate::{
    lenient::{Items, shape},
    library::{AlbumSummary, ArtistSummary, PlaylistSummary},
    net::{self, AlbumKey, Artist, Image, Named, Track, covers, with_prefix},
    pathfinder,
    podcasts::EpisodeData,
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
    Ok(parse(data))
}

// ---- what a `searchDesktop` answer is read for -------------------------------------

shape! {
    pub(crate) struct Data {
        search: Search,
    }
}

shape! {
    pub(crate) struct Search {
        tracks: Items<TrackItem>,
        artists: Items<Artist>,
        albums: Items<Album>,
        playlists: Items<Playlist>,
        episodes: Items<EpisodeItem>,
    }
}

shape! {
    pub(crate) struct TrackItem {
        track: Track,
    }
}

shape! {
    pub(crate) struct EpisodeItem {
        episode: EpisodeData,
    }
}

shape! {
    pub(crate) struct AlbumArtist {
        profile: Named,
    }
}

shape! {
    pub(crate) struct Year {
        year: Option<i64>,
    }
}

shape! {
    pub(crate) struct Album {
        uri: Option<String>,
        name: Option<String>,
        artists: Items<AlbumArtist>,
        date: Year,
        cover_art: Image,
    }
}

shape! {
    pub(crate) struct Playlist {
        uri: Option<String>,
        name: Option<String>,
        owner: Named,
        images: Items<Image>,
    }
}

/// Reads a `searchDesktop` answer's `data`. Anything missing is skipped,
/// never a reason to fail the whole search.
pub(crate) fn parse(data: Data) -> SearchResults {
    let search = data.search;
    SearchResults {
        tracks: search
            .tracks
            .items
            .into_iter()
            .filter_map(|i| net::track(i.track, AlbumKey::Album))
            .collect(),
        artists: search
            .artists
            .items
            .into_iter()
            .filter_map(net::artist)
            .collect(),
        albums: search.albums.items.into_iter().filter_map(album).collect(),
        playlists: search
            .playlists
            .items
            .into_iter()
            .filter_map(playlist)
            .collect(),
        episodes: search
            .episodes
            .items
            .into_iter()
            .filter_map(|i| crate::podcasts::episode(i.episode))
            .collect(),
    }
}

fn album(a: Album) -> Option<AlbumSummary> {
    Some(AlbumSummary {
        uri: with_prefix(a.uri, "spotify:album:")?,
        name: a.name.unwrap_or_default(),
        artists: a
            .artists
            .items
            .into_iter()
            .map(|x| x.profile.name.unwrap_or_default())
            .collect(),
        year: a.date.year.map(|y| y as i32),
        covers: covers(a.cover_art),
    })
}

fn playlist(p: Playlist) -> Option<PlaylistSummary> {
    Some(PlaylistSummary {
        uri: with_prefix(p.uri, "spotify:playlist:")?,
        name: p.name.unwrap_or_default(),
        owner: p.owner.name.unwrap_or_default(),
        covers: p
            .images
            .items
            .into_iter()
            .next()
            .map(covers)
            .unwrap_or_default(),
        description: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::lenient::read;

    fn fixture() -> Value {
        json!({"search": {
            "__typename": "SearchResponse",
            "tracks": {"totalCount": 1000, "items": [
                {"track": {
                    "__typename": "Track",
                    "uri": "spotify:track:t1", "name": "Airbag",
                    "contentRating": {"label": "NONE"},
                    "artists": {"items": [{"profile": {"name": "Radiohead"}, "uri": "spotify:artist:a1"}]},
                    "album": {"name": "OK Computer", "uri": "spotify:album:b1",
                              "coverArt": {"extractedColors": {"colorDark": {"hex": "#535353"}},
                                           "sources": [
                                  {"url": "https://i.scdn.co/image/l", "width": 640, "height": 640},
                                  {"url": "https://i.scdn.co/image/s", "width": 64, "height": 64}]}},
                    "duration": {"totalMilliseconds": 284000},
                    "playability": {"playable": true, "reason": "PLAYABLE"}}},
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
                "images": {"items": [{"sources": [{"url": "https://i.scdn.co/image/q", "width": null}]}]}}]},
            "users": {"items": [{"data": {"uri": "spotify:user:x", "displayName": "Someone"}}]},
            "genres": {"items": [], "totalCount": 0}
        }})
    }

    #[test]
    fn tracks_come_with_artists_album_duration_and_covers() {
        let r = parse(read(fixture()));
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
        assert_eq!(t.album_uri, "spotify:album:b1");
        assert_eq!(t.duration_ms, 284_000);
        assert_eq!(
            t.covers.iter().map(|c| c.width).collect::<Vec<_>>(),
            vec![64, 640]
        );
    }

    #[test]
    fn episodes_come_along_without_their_show() {
        let r = parse(read(fixture()));
        assert_eq!(r.episodes.len(), 1);
        let e = &r.episodes[0];
        assert_eq!(e.name, "A Landslide Victory for Democrats");
        assert_eq!(e.released, Some((2026, 11, 4)));
        assert_eq!(e.show, "", "this answer does not name the show");
        assert!(!r.is_empty());
    }

    #[test]
    fn albums_artists_and_playlists() {
        let r = parse(read(fixture()));
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
        assert!(parse(read(json!(null))).is_empty());
        assert!(parse(read(json!({"search": {"tracks": {"items": "nope"}}}))).is_empty());
    }

    #[test]
    fn a_field_of_the_wrong_kind_costs_only_that_field() {
        let r = parse(read(json!({"search": {
            "tracks": {"items": [
                {"track": {"uri": "spotify:track:t1", "name": 7, "artists": "Radiohead",
                           "album": [], "duration": {"totalMilliseconds": "long"}}},
                "not a track"
            ]},
            "albums": {"items": [{"uri": "spotify:album:b1", "name": "OK Computer",
                                  "date": {"year": "1997"}, "coverArt": null}]},
            "playlists": {"items": [{"uri": "spotify:playlist:p1", "images": {"items": [null]}}]}
        }})));
        assert_eq!(r.tracks.len(), 1);
        let t = &r.tracks[0];
        assert_eq!((t.title.as_str(), t.duration_ms), ("", 0));
        assert!(t.artists.is_empty() && t.covers.is_empty());
        assert_eq!(r.albums[0].name, "OK Computer");
        assert_eq!(r.albums[0].year, None);
        assert!(r.playlists[0].covers.is_empty());
    }
}
