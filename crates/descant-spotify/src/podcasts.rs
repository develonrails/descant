//! Podcasts: shows and their episodes, through the session (spike S7).
//!
//! The Web API refuses this client (S3), so shows, episode lists and search
//! come from the web player's own pathfinder operations, which also name the
//! listener's place in each episode. Followed shows and Your Episodes are the
//! collection sets `show` and `listenlater`, described through extended
//! metadata like the rest of the library. Audiobooks are left out: librespot
//! cannot play them.

use std::collections::HashMap;

use descant_core::{
    podcast::{self, Date, Progress},
    track::Cover,
};
use librespot_core::{Error, session::Session};
use librespot_protocol::{extension_kind::ExtensionKind, metadata};
use serde_json::json;

use crate::{
    lenient::{Data, Items, shape},
    library::{collection_items, cover, extended, gid_uri, newest_first},
    net::{Duration, Image, Named, covers, with_prefix},
    pathfinder,
};

/// A show, as a card shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShowSummary {
    pub uri: String,
    pub name: String,
    pub publisher: String,
    /// Smallest first.
    pub covers: Vec<Cover>,
}

/// One episode, as a row shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    pub uri: String,
    pub name: String,
    /// Empty where the answer does not name it (music search).
    pub show: String,
    pub show_uri: String,
    pub duration_ms: u32,
    /// The release date, in UTC; `None` when Spotify names none.
    pub released: Option<Date>,
    /// Smallest first.
    pub covers: Vec<Cover>,
    /// Where the listener is in it; not started where Spotify does not say.
    pub progress: Progress,
}

/// The top of a show's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowPage {
    pub show: ShowSummary,
    pub description: String,
    /// Whether the listener follows it.
    pub followed: bool,
}

/// One page of a show's episodes, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EpisodePage {
    pub episodes: Vec<Episode>,
    /// All the show's episodes, not only this page's.
    pub total: u32,
    /// The offset of the next page, if there is one.
    pub next: Option<u32>,
}

pub async fn show(session: &Session, uri: &str) -> Result<ShowPage, Error> {
    let data: ShowAnswer =
        pathfinder::query(session, &pathfinder::SHOW_METADATA, json!({"uri": uri})).await?;
    parse_show(data.podcast_union_v2).ok_or_else(|| Error::not_found(format!("no show {uri}")))
}

pub async fn episodes(
    session: &Session,
    uri: &str,
    offset: u32,
    limit: u32,
) -> Result<EpisodePage, Error> {
    let data: EpisodesAnswer = pathfinder::query(
        session,
        &pathfinder::PODCAST_EPISODES,
        json!({"uri": uri, "offset": offset, "limit": limit}),
    )
    .await?;
    Ok(parse_episodes(data.podcast_union_v2.episodes_v2))
}

/// Shows for `term`, up to `limit`. Episodes come with the music search.
pub async fn search_shows(
    session: &Session,
    term: &str,
    limit: u32,
) -> Result<Vec<ShowSummary>, Error> {
    let data: ShowsAnswer = pathfinder::query(
        session,
        &pathfinder::SEARCH_PODCASTS,
        json!({"searchTerm": term, "offset": 0, "limit": limit, "numberOfTopResults": 5,
               "includeAudiobooks": false, "includePreReleases": false}),
    )
    .await?;
    Ok(parse_shows(data.search_v2))
}

/// The shows the listener follows, most recently followed first.
pub async fn followed(session: &Session) -> Result<Vec<ShowSummary>, Error> {
    let items = collection_items(session, "show").await?;
    let uris = newest_first(&items, "spotify:show:");
    let shows: HashMap<String, metadata::Show> =
        extended(session, ExtensionKind::SHOW_V4, &uris).await?;
    Ok(uris
        .iter()
        .filter_map(|u| {
            shows
                .get(u)
                .filter(|s| !s.is_audiobook())
                .map(|s| show_summary(u, s))
        })
        .collect())
}

/// Your Episodes, most recently saved first. Extended metadata names no
/// played state, so these read as not started.
pub async fn saved_episodes(session: &Session) -> Result<Vec<Episode>, Error> {
    let items = collection_items(session, "listenlater").await?;
    let uris = newest_first(&items, "spotify:episode:");
    let found: HashMap<String, metadata::Episode> =
        extended(session, ExtensionKind::EPISODE_V4, &uris).await?;
    Ok(uris
        .iter()
        .filter_map(|u| found.get(u).map(|e| episode_from_metadata(u, e)))
        .collect())
}

/// The URIs in a collection set, for marking what is followed or saved.
pub async fn saved_uris(session: &Session, set: &str) -> Result<Vec<String>, Error> {
    let prefix = if set == "show" {
        "spotify:show:"
    } else {
        "spotify:episode:"
    };
    Ok(newest_first(&collection_items(session, set).await?, prefix))
}

/// The newest `per_show` episodes of each show, with the listener's place in
/// them: what Home's Continue listening and New episodes are made of. One
/// request per show; a refusal ends the round with what was read so far.
pub async fn latest(session: &Session, shows: &[ShowSummary], per_show: u32) -> Vec<Episode> {
    let mut all = Vec::new();
    for show in shows {
        match episodes(session, &show.uri, 0, per_show).await {
            Ok(page) => all.extend(page.episodes),
            Err(_) => break,
        }
    }
    all
}

// ---- what pathfinder's podcast answers are read for --------------------------------

shape! {
    pub(crate) struct ShowAnswer {
        podcast_union_v2: ShowData,
    }
}

shape! {
    /// `podcastUnionV2` of `queryShowMetadataV2`.
    pub(crate) struct ShowData {
        uri: Option<String>,
        name: Option<String>,
        publisher: Named,
        cover_art: Image,
        description: Option<String>,
        saved: Option<bool>,
    }
}

shape! {
    pub(crate) struct EpisodesAnswer {
        podcast_union_v2: EpisodesUnion,
    }
}

shape! {
    pub(crate) struct EpisodesUnion {
        episodes_v2: EpisodesV2,
    }
}

shape! {
    /// `episodesV2` of `queryPodcastEpisodes`.
    pub(crate) struct EpisodesV2 {
        items: Vec<EpisodeEntity>,
        total_count: Option<u64>,
        paging_info: PagingInfo,
    }
}

shape! {
    pub(crate) struct EpisodeEntity {
        entity: Data<EpisodeData>,
    }
}

shape! {
    pub(crate) struct PagingInfo {
        next_offset: Option<u64>,
    }
}

shape! {
    pub(crate) struct ShowsAnswer {
        search_v2: SearchV2,
    }
}

shape! {
    /// `searchV2` of `searchPodcasts`.
    pub(crate) struct SearchV2 {
        podcasts: Items<Data<ShowCard>>,
    }
}

shape! {
    pub(crate) struct ShowCard {
        uri: Option<String>,
        name: Option<String>,
        publisher: Named,
        cover_art: Image,
    }
}

shape! {
    /// A pathfinder episode, as show pages and search name it.
    pub(crate) struct EpisodeData {
        uri: Option<String>,
        name: Option<String>,
        duration: Duration,
        played_state: PlayedState,
        podcast_v2: Data<ShowLink>,
        release_date: ReleaseDate,
        cover_art: Image,
    }
}

shape! {
    pub(crate) struct PlayedState {
        play_position_milliseconds: Option<u64>,
        state: Option<String>,
    }
}

shape! {
    pub(crate) struct ShowLink {
        uri: Option<String>,
        name: Option<String>,
    }
}

shape! {
    pub(crate) struct ReleaseDate {
        iso_string: Option<String>,
    }
}

// ---- pure helpers ----------------------------------------------------------

pub(crate) fn parse_show(d: ShowData) -> Option<ShowPage> {
    Some(ShowPage {
        show: ShowSummary {
            uri: with_prefix(d.uri, "spotify:show:")?,
            name: d.name.unwrap_or_default(),
            publisher: d.publisher.name.unwrap_or_default(),
            covers: covers(d.cover_art),
        },
        description: d.description.unwrap_or_default(),
        followed: d.saved.unwrap_or(false),
    })
}

pub(crate) fn parse_episodes(v: EpisodesV2) -> EpisodePage {
    EpisodePage {
        episodes: v
            .items
            .into_iter()
            .filter_map(|i| episode(i.entity.data))
            .collect(),
        total: v.total_count.unwrap_or(0) as u32,
        next: v.paging_info.next_offset.map(|n| n as u32),
    }
}

pub(crate) fn parse_shows(v: SearchV2) -> Vec<ShowSummary> {
    v.podcasts
        .items
        .into_iter()
        .filter_map(|Data { data: d }| {
            Some(ShowSummary {
                uri: with_prefix(d.uri, "spotify:show:")?,
                name: d.name.unwrap_or_default(),
                publisher: d.publisher.name.unwrap_or_default(),
                covers: covers(d.cover_art),
            })
        })
        .collect()
}

/// A pathfinder episode; anything that is not one is left out.
pub(crate) fn episode(d: EpisodeData) -> Option<Episode> {
    let uri = with_prefix(d.uri, "spotify:episode:")?;
    let duration_ms = d.duration.total_milliseconds.unwrap_or(0) as u32;
    let played = d.played_state;
    let position = played.play_position_milliseconds.unwrap_or(0) as u32;
    let show = d.podcast_v2.data;
    Some(Episode {
        uri,
        name: d.name.unwrap_or_default(),
        show: show.name.unwrap_or_default(),
        show_uri: show.uri.unwrap_or_default(),
        duration_ms,
        released: d.release_date.iso_string.as_deref().and_then(iso_date),
        covers: covers(d.cover_art),
        progress: podcast::progress(
            played.state.as_deref().unwrap_or_default(),
            position,
            duration_ms,
        ),
    })
}

/// The date of an ISO 8601 timestamp such as `2026-11-04T10:51:00Z`.
pub(crate) fn iso_date(iso: &str) -> Option<Date> {
    let date = iso.split('T').next()?;
    let mut parts = date.split('-').map(str::parse::<u32>);
    let (year, month, day) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then_some((year as i32, month, day))
}

pub fn show_summary(uri: &str, show: &metadata::Show) -> ShowSummary {
    ShowSummary {
        uri: uri.to_string(),
        name: show.name().to_string(),
        publisher: show.publisher().to_string(),
        covers: sorted(show.cover_image.image.iter().filter_map(cover).collect()),
    }
}

pub fn episode_from_metadata(uri: &str, e: &metadata::Episode) -> Episode {
    let released = e
        .publish_time
        .as_ref()
        .filter(|d| d.year() > 0)
        .map(|d| (d.year(), d.month().max(1) as u32, d.day().max(1) as u32));
    Episode {
        uri: uri.to_string(),
        name: e.name().to_string(),
        show: e.show.name().to_string(),
        show_uri: gid_uri("show", e.show.gid()).unwrap_or_default(),
        duration_ms: e.duration().max(0) as u32,
        released,
        covers: sorted(e.cover_image.image.iter().filter_map(cover).collect()),
        progress: Progress::NotStarted,
    }
}

fn sorted(mut covers: Vec<Cover>) -> Vec<Cover> {
    covers.sort_by_key(|c| c.width);
    covers
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::lenient::read;

    /// An episode as `queryPodcastEpisodes` and search name it (trimmed).
    fn daily(state: &str, position: u64) -> Value {
        json!({
            "uri": "spotify:episode:5UxOyOLAC4usAfIblpKROk",
            "name": "A Landslide Victory for Democrats",
            "description": "On Tuesday night, the blue wave…",
            "duration": {"totalMilliseconds": 2079973},
            "releaseDate": {"isoString": "2026-11-04T10:51:00Z", "precision": "MINUTE"},
            "playedState": {"playPositionMilliseconds": position, "state": state},
            "podcastV2": {"data": {"__typename": "Podcast", "name": "The Daily",
                                   "uri": "spotify:show:3IM0lmZxpFAY7CwMuv9H4g"}},
            "coverArt": {"sources": [
                {"height": 640, "url": "https://i.scdn.co/image/big", "width": 640},
                {"height": 64, "url": "https://i.scdn.co/image/small", "width": 64}]}
        })
    }

    #[test]
    fn an_episode_names_its_show_its_date_and_the_place_reached() {
        let e = episode(read(daily("IN_PROGRESS", 600_000))).unwrap();
        assert_eq!(e.name, "A Landslide Victory for Democrats");
        assert_eq!(e.show, "The Daily");
        assert_eq!(e.show_uri, "spotify:show:3IM0lmZxpFAY7CwMuv9H4g");
        assert_eq!(e.duration_ms, 2_079_973);
        assert_eq!(e.released, Some((2026, 11, 4)));
        assert_eq!(e.covers[0].width, 64, "smallest first");
        assert_eq!(
            e.progress,
            Progress::InProgress {
                position_ms: 600_000,
                left_ms: 2_079_973 - 600_000
            }
        );
    }

    #[test]
    fn what_is_not_an_episode_is_left_out() {
        assert_eq!(episode(read(json!({"uri": "spotify:track:x"}))), None);
        assert_eq!(episode(read(Value::Null)), None);
    }

    #[test]
    fn an_episode_page_counts_all_and_knows_where_the_next_starts() {
        let v = json!({
            "items": [{"entity": {"data": daily("NOT_STARTED", 0)}},
                      {"entity": {"data": {"uri": "spotify:track:not-an-episode"}}}],
            "pagingInfo": {"nextOffset": 8},
            "totalCount": 3027
        });
        let page = parse_episodes(read(v));
        assert_eq!(page.episodes.len(), 1);
        assert_eq!(page.total, 3027);
        assert_eq!(page.next, Some(8));
        assert_eq!(
            parse_episodes(read(json!({"items": []}))).next,
            None,
            "the last page"
        );
    }

    #[test]
    fn a_show_page_says_who_makes_it_and_whether_it_is_followed() {
        let v = json!({
            "__typename": "Podcast",
            "uri": "spotify:show:3IM0lmZxpFAY7CwMuv9H4g",
            "name": "The Daily",
            "publisher": {"name": "The New York Times"},
            "description": "This is what the news should sound like.",
            "coverArt": {"sources": [{"url": "https://i.scdn.co/image/c", "width": 300}]},
            "saved": true
        });
        let page = parse_show(read(v)).unwrap();
        assert_eq!(page.show.name, "The Daily");
        assert_eq!(page.show.publisher, "The New York Times");
        assert_eq!(page.description, "This is what the news should sound like.");
        assert!(page.followed);
        assert_eq!(parse_show(read(json!({"__typename": "NotFound"}))), None);
    }

    #[test]
    fn a_podcast_search_finds_shows() {
        let v = json!({
            "podcasts": {"items": [{"data": {"uri": "spotify:show:73vZPMVjxTqC02OYZdcCr7",
                "name": "NRC Vandaag", "publisher": {"name": "NRC"},
                "coverArt": {"sources": [{"url": "https://i.scdn.co/image/n", "width": 64}]}}},
                {"data": {"uri": "spotify:audiobook:x", "name": "Not a show"}}]},
            "query": {}
        });
        let shows = parse_shows(read(v));
        assert_eq!(shows.len(), 1);
        assert_eq!(shows[0].name, "NRC Vandaag");
        assert_eq!(shows[0].publisher, "NRC");
        assert!(parse_shows(read(json!({"podcasts": null}))).is_empty());
    }

    #[test]
    fn iso_timestamps_give_their_date() {
        assert_eq!(iso_date("2026-11-04T10:51:00Z"), Some((2026, 11, 4)));
        assert_eq!(iso_date("2025-01-31"), Some((2025, 1, 31)));
        assert_eq!(iso_date("2026-13-01T00:00:00Z"), None);
        assert_eq!(iso_date("soon"), None);
    }

    #[test]
    fn extended_metadata_describes_a_saved_episode() {
        let mut e = metadata::Episode::new();
        e.set_name("Want to Enjoy Music More? Go For a Run.".into());
        e.set_duration(2_172_604);
        let mut date = metadata::Date::new();
        date.set_year(2026);
        date.set_month(11);
        date.set_day(1);
        e.publish_time = Some(date).into();
        let mut show = metadata::Show::new();
        show.set_name("The Daily".into());
        show.set_gid(vec![0; 16]);
        e.show = Some(show).into();
        let got = episode_from_metadata("spotify:episode:a", &e);
        assert_eq!(got.name, "Want to Enjoy Music More? Go For a Run.");
        assert_eq!(got.show, "The Daily");
        assert!(got.show_uri.starts_with("spotify:show:"));
        assert_eq!(got.duration_ms, 2_172_604);
        assert_eq!(got.released, Some((2026, 11, 1)));
        assert_eq!(got.progress, Progress::NotStarted);
        let undated = episode_from_metadata("spotify:episode:b", &metadata::Episode::new());
        assert_eq!(undated.released, None);
    }
}
