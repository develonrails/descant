//! The user's library, read through the playback session.
//!
//! No Web API: the playlists come from the session's rootlist, a playlist's
//! rows from spclient in pages, and the rows' titles, artists and covers from
//! one batched extended-metadata request per page. This is what spike S3
//! found to work, and how Spotifast (MIT) reads the same data.

use std::collections::HashMap;

use descant_core::track::{Cover, TrackInfo};
use librespot_core::{Error, SpotifyId, session::Session};
use librespot_protocol::{
    extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery},
    extension_kind::ExtensionKind,
    metadata::{Album, AlbumGroup, Artist, Image, Track, image::Size},
    playlist4_external::{ListAttributes, SelectedListContent},
};
use protobuf::{EnumOrUnknown, Message};

use crate::collection::{PageRequest, PageResponse};

/// A playlist in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistSummary {
    pub uri: String,
    pub name: String,
    pub owner: String,
    /// Cover art, smallest first; empty only when the list carries no picture.
    pub covers: Vec<Cover>,
    /// Spotify's own blurb (Made for You), plain text; often empty.
    pub description: String,
}

/// One page of a playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistPage {
    pub name: String,
    pub owner: String,
    /// Rows in the whole playlist.
    pub total: u32,
    pub covers: Vec<Cover>,
    /// The rows of this page that Spotify could describe, in order.
    pub tracks: Vec<TrackInfo>,
}

/// An album in a grid or list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumSummary {
    pub uri: String,
    pub name: String,
    pub artists: Vec<String>,
    pub year: Option<i32>,
    pub covers: Vec<Cover>,
}

/// An artist in a grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistSummary {
    pub uri: String,
    pub name: String,
    pub covers: Vec<Cover>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumPage {
    pub album: AlbumSummary,
    pub tracks: Vec<TrackInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistPage {
    pub artist: ArtistSummary,
    pub top_tracks: Vec<TrackInfo>,
    pub albums: Vec<AlbumSummary>,
    pub singles: Vec<AlbumSummary>,
}

/// The account's playlists, in the order the user keeps them. Folders are
/// flattened for now.
pub async fn playlists(session: &Session) -> Result<Vec<PlaylistSummary>, Error> {
    let bytes = session.spclient().get_rootlist(0, Some(500)).await?;
    let content = SelectedListContent::parse_from_bytes(&bytes)?;
    Ok(summaries(&content))
}

/// Rows `offset..offset + limit` of a playlist, described.
pub async fn playlist(
    session: &Session,
    uri: &str,
    offset: u32,
    limit: u32,
) -> Result<PlaylistPage, Error> {
    let id = crate::net::playlist_id(uri)?;
    let endpoint = format!(
        "/playlist/v2/playlist/{id}?decorate=attributes,length,owner&from={offset}&length={limit}"
    );
    let bytes = session
        .spclient()
        .request(&http::Method::GET, &endpoint, None, None)
        .await?;
    let content = SelectedListContent::parse_from_bytes(&bytes)?;
    let uris = track_uris(&content);
    let tracks = tracks(session, &uris).await?;
    Ok(PlaylistPage {
        name: content.attributes.name().to_string(),
        owner: content.owner_username().to_string(),
        total: content.length().max(0) as u32,
        covers: playlist_covers(&content.attributes),
        tracks,
    })
}

/// Titles, artists, albums and covers for tracks, in the given order;
/// tracks Spotify could not describe are left out.
pub async fn tracks(session: &Session, uris: &[String]) -> Result<Vec<TrackInfo>, Error> {
    let found: HashMap<String, Track> = extended(session, ExtensionKind::TRACK_V4, uris).await?;
    Ok(uris
        .iter()
        .filter_map(|u| found.get(u).map(|t| track_info(u, t)))
        .collect())
}

/// One extended-metadata message per URI, batched.
pub(crate) async fn extended<M: Message>(
    session: &Session,
    kind: ExtensionKind,
    uris: &[String],
) -> Result<HashMap<String, M>, Error> {
    let mut found = HashMap::new();
    for chunk in uris.chunks(100) {
        let response = session
            .spclient()
            .get_extended_metadata(batch(kind, chunk))
            .await?;
        for array in &response.extended_metadata {
            if array.extension_kind.enum_value() != Ok(kind) {
                continue;
            }
            for data in &array.extension_data {
                let Some(any) = data.extension_data.as_ref() else {
                    continue;
                };
                if let Ok(message) = M::parse_from_bytes(&any.value) {
                    found.insert(data.entity_uri.clone(), message);
                }
            }
        }
    }
    Ok(found)
}

fn batch(kind: ExtensionKind, uris: &[String]) -> BatchedEntityRequest {
    let mut request = BatchedEntityRequest::new();
    request.entity_request = uris
        .iter()
        .map(|uri| EntityRequest {
            entity_uri: uri.clone(),
            query: vec![ExtensionQuery {
                extension_kind: EnumOrUnknown::new(kind),
                ..Default::default()
            }],
            ..Default::default()
        })
        .collect();
    request
}

// ---- collection: Liked Songs, saved albums, followed artists ----------------

/// One page of a collection set (`collection` holds liked tracks and saved
/// albums, `artist` followed artists).
async fn collection_page(
    session: &Session,
    set: &str,
    token: &str,
    limit: i32,
) -> Result<PageResponse, Error> {
    let request = PageRequest {
        username: session.username(),
        set: set.to_string(),
        pagination_token: token.to_string(),
        limit,
    };
    let kind = http::HeaderValue::from_static(crate::net::COLLECTION_TYPE);
    let mut headers = http::HeaderMap::new();
    headers.insert(http::header::CONTENT_TYPE, kind.clone());
    headers.insert(http::header::ACCEPT, kind);
    let bytes = session
        .spclient()
        .request(
            &http::Method::POST,
            "/collection/v2/paging",
            Some(headers),
            Some(&request.encode()),
        )
        .await?;
    Ok(PageResponse::decode(&bytes)?)
}

/// Every saved item of a collection set, all pages.
async fn collection_items(
    session: &Session,
    set: &str,
) -> Result<Vec<crate::collection::CollectionItem>, Error> {
    let (mut items, mut token) = (Vec::new(), String::new());
    for _ in 0..50 {
        let page = collection_page(session, set, &token, 500).await?;
        let next = next_token(&page, &token);
        items.extend(page.items);
        match next {
            Some(next) => token = next,
            None => break,
        }
    }
    Ok(items)
}

/// Liked Songs and saved albums, each most recently added first, from one
/// read of the `collection` set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collection {
    pub tracks: Vec<String>,
    pub albums: Vec<String>,
}

/// Spotify pages a set in its own order, so all pages are read, then sorted.
pub async fn collection(session: &Session) -> Result<Collection, Error> {
    let items = collection_items(session, "collection").await?;
    Ok(Collection {
        tracks: newest_first(&items, "spotify:track:"),
        albums: newest_first(&items, "spotify:album:"),
    })
}

/// Albums described, in the order given.
pub async fn albums(session: &Session, uris: &[String]) -> Result<Vec<AlbumSummary>, Error> {
    let albums: HashMap<String, Album> = extended(session, ExtensionKind::ALBUM_V4, uris).await?;
    Ok(uris
        .iter()
        .filter_map(|u| albums.get(u).map(|a| album_summary(u, a)))
        .collect())
}

pub async fn followed_artists(session: &Session) -> Result<Vec<ArtistSummary>, Error> {
    let items = collection_items(session, "artist").await?;
    let uris = newest_first(&items, "spotify:artist:");
    let artists: HashMap<String, Artist> =
        extended(session, ExtensionKind::ARTIST_V4, &uris).await?;
    let mut list: Vec<ArtistSummary> = uris
        .iter()
        .filter_map(|u| artists.get(u).map(|a| artist_summary(u, a)))
        .collect();
    list.sort_by_key(|a| a.name.to_lowercase());
    Ok(list)
}

// ---- album and artist pages -----------------------------------------------

pub async fn album(session: &Session, uri: &str) -> Result<AlbumPage, Error> {
    let albums: HashMap<String, Album> =
        extended(session, ExtensionKind::ALBUM_V4, &[uri.to_string()]).await?;
    let album = albums.get(uri).ok_or_else(|| Error::not_found("album"))?;
    Ok(AlbumPage {
        album: album_summary(uri, album),
        tracks: tracks(session, &album_track_uris(album)).await?,
    })
}

/// An artist with its top tracks, albums, and the newest `singles_shown` singles.
pub async fn artist(
    session: &Session,
    uri: &str,
    singles_shown: usize,
) -> Result<ArtistPage, Error> {
    let artists: HashMap<String, Artist> =
        extended(session, ExtensionKind::ARTIST_V4, &[uri.to_string()]).await?;
    let artist = artists.get(uri).ok_or_else(|| Error::not_found("artist"))?;
    let albums = group_uris(&artist.album_group);
    let mut singles = group_uris(&artist.single_group);
    singles.truncate(singles_shown);
    let all: Vec<String> = albums.iter().chain(&singles).cloned().collect();
    let top = top_track_uris(artist, &session.country());
    let (described, top_tracks) = tokio::join!(
        extended::<Album>(session, ExtensionKind::ALBUM_V4, &all),
        tracks(session, &top)
    );
    let described = described?;
    let summaries = |uris: &[String]| -> Vec<AlbumSummary> {
        uris.iter()
            .filter_map(|u| described.get(u).map(|a| album_summary(u, a)))
            .collect()
    };
    Ok(ArtistPage {
        artist: artist_summary(uri, artist),
        top_tracks: top_tracks?,
        albums: summaries(&albums),
        singles: summaries(&singles),
    })
}

// ---- pure helpers ------------------------------------------------------------

/// The saved items with `prefix`, most recently added first; items added at
/// the same moment keep Spotify's order.
pub fn newest_first(items: &[crate::collection::CollectionItem], prefix: &str) -> Vec<String> {
    let mut saved: Vec<&crate::collection::CollectionItem> = items
        .iter()
        .filter(|item| !item.is_removed && item.uri.starts_with(prefix))
        .collect();
    saved.sort_by_key(|item| std::cmp::Reverse(item.added_at)); // stable
    saved.into_iter().map(|item| item.uri.clone()).collect()
}

/// The token of the next page, unless this was the last (or Spotify repeats itself).
pub fn next_token(page: &PageResponse, current: &str) -> Option<String> {
    let next = page.next_page_token.as_str();
    (!next.is_empty() && next != current).then(|| next.to_string())
}

fn gid_uri(kind: &str, gid: &[u8]) -> Option<String> {
    SpotifyId::from_raw(gid)
        .ok()
        .map(|id| format!("spotify:{kind}:{}", id.to_base62()))
}

fn covers_of(group: &[Image], loose: &[Image]) -> Vec<Cover> {
    let mut covers: Vec<Cover> = group.iter().chain(loose).filter_map(cover).collect();
    covers.sort_by_key(|c| c.width);
    covers.dedup_by_key(|c| c.width);
    covers
}

pub fn album_summary(uri: &str, album: &Album) -> AlbumSummary {
    AlbumSummary {
        uri: uri.to_string(),
        name: album.name().to_string(),
        artists: album.artist.iter().map(|a| a.name().to_string()).collect(),
        year: album.date.as_ref().map(|d| d.year()).filter(|y| *y > 0),
        covers: covers_of(&album.cover_group.image, &album.cover),
    }
}

pub fn artist_summary(uri: &str, artist: &Artist) -> ArtistSummary {
    ArtistSummary {
        uri: uri.to_string(),
        name: artist.name().to_string(),
        covers: covers_of(&artist.portrait_group.image, &artist.portrait),
    }
}

pub fn album_track_uris(album: &Album) -> Vec<String> {
    album
        .disc
        .iter()
        .flat_map(|disc| &disc.track)
        .filter_map(|track| gid_uri("track", track.gid()))
        .collect()
}

/// The artist's top tracks for `country`, else for the first country listed.
pub fn top_track_uris(artist: &Artist, country: &str) -> Vec<String> {
    let list = artist
        .top_track
        .iter()
        .find(|t| t.country() == country)
        .or_else(|| artist.top_track.first());
    list.map(|t| {
        t.track
            .iter()
            .filter_map(|tr| gid_uri("track", tr.gid()))
            .collect()
    })
    .unwrap_or_default()
}

/// One album per group: a group holds the versions of the same release.
pub fn group_uris(groups: &[AlbumGroup]) -> Vec<String> {
    groups
        .iter()
        .filter_map(|g| g.album.first())
        .filter_map(|a| gid_uri("album", a.gid()))
        .collect()
}

/// The playlists in a rootlist, skipping folder markers.
pub fn summaries(content: &SelectedListContent) -> Vec<PlaylistSummary> {
    let Some(contents) = content.contents.as_ref() else {
        return vec![];
    };
    contents
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.uri().starts_with("spotify:playlist:"))
        .map(|(i, item)| {
            let meta = contents.meta_items.get(i);
            PlaylistSummary {
                uri: item.uri().to_string(),
                name: meta
                    .map(|m| m.attributes.name().to_string())
                    .unwrap_or_default(),
                owner: meta
                    .map(|m| m.owner_username().to_string())
                    .unwrap_or_default(),
                covers: meta
                    .map(|m| playlist_covers(&m.attributes))
                    .unwrap_or_default(),
                description: String::new(),
            }
        })
        .collect()
}

/// The track rows of a playlist page; episodes and local files are skipped.
pub fn track_uris(content: &SelectedListContent) -> Vec<String> {
    content
        .contents
        .as_ref()
        .map(|contents| {
            contents
                .items
                .iter()
                .map(|item| item.uri())
                .filter(|uri| uri.starts_with("spotify:track:"))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

pub fn track_info(uri: &str, track: &Track) -> TrackInfo {
    let mut covers: Vec<Cover> = track
        .album
        .cover_group
        .image
        .iter()
        .filter_map(cover)
        .collect();
    covers.sort_by_key(|c| c.width);
    TrackInfo {
        uri: uri.to_string(),
        title: track.name().to_string(),
        artists: track.artist.iter().map(|a| a.name().to_string()).collect(),
        artist_uris: track
            .artist
            .iter()
            .filter_map(|a| gid_uri("artist", a.gid()))
            .collect(),
        album: track.album.name().to_string(),
        album_uri: gid_uri("album", track.album.gid()).unwrap_or_default(),
        duration_ms: track.duration().max(0) as u32,
        covers,
    }
}

/// A playlist's cover, smallest first: the sizes the list names, or else the
/// single picture every list carries. A playlist without an uploaded cover
/// has only that second field — Spotify draws it as a mosaic of the first
/// albums — so reading the sizes alone leaves most playlists blank.
/// Spotifast (MIT) reads the same two fields.
pub fn playlist_covers(attributes: &ListAttributes) -> Vec<Cover> {
    let mut covers: Vec<Cover> = attributes
        .picture_size
        .iter()
        .filter_map(|p| {
            let width = match p.target_name() {
                "small" => 60,
                "default" => 300,
                "large" => 640,
                _ => return None, // "xlarge" repeats "large"
            };
            Some(Cover {
                url: image_url(p.url())?,
                width,
            })
        })
        .collect();
    covers.sort_by_key(|c| c.width);
    if !covers.is_empty() {
        return covers;
    }
    let picture = attributes.picture();
    if picture.is_empty() {
        return vec![];
    }
    // The CDN serves this one at whatever size it was made; ask for it as the
    // largest, so a small row scales down rather than up.
    vec![Cover {
        url: format!("{IMAGE_CDN}{}", hex(picture)),
        width: 640,
    }]
}

/// A picture as a list refers to it: a web address (a mosaic Spotify already
/// composed, say), or an image URI naming a file on the CDN.
fn image_url(reference: &str) -> Option<String> {
    if reference.starts_with("https://") || reference.starts_with("http://") {
        return Some(reference.to_string());
    }
    reference
        .strip_prefix("spotify:image:")
        .map(|hex| format!("{IMAGE_CDN}{hex}"))
}

/// A file id as the CDN spells it.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Spotify's image CDN, which serves a cover by its file id in hex.
const IMAGE_CDN: &str = "https://i.scdn.co/image/";

/// A cover from extended metadata, at the size the metadata names.
pub fn cover(image: &Image) -> Option<Cover> {
    let file = image.file_id();
    if file.is_empty() {
        return None;
    }
    let width = match image.width() {
        w if w > 0 => w as u32,
        _ => match image.size() {
            Size::SMALL => 64,
            Size::DEFAULT => 300,
            Size::LARGE => 640,
            Size::XLARGE => 1280,
        },
    };
    Some(Cover {
        url: format!("{IMAGE_CDN}{}", hex(file)),
        width,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_protocol::{
        metadata::{Album, Artist, ImageGroup},
        playlist4_external::{Item, ListAttributes, ListItems, MetaItem},
    };
    use protobuf::MessageField;

    fn item(uri: &str) -> Item {
        let mut i = Item::new();
        i.set_uri(uri.into());
        i
    }

    fn meta(name: &str, owner: &str, length: i32) -> MetaItem {
        let mut attributes = ListAttributes::new();
        attributes.set_name(name.into());
        let mut m = MetaItem::new();
        m.attributes = MessageField::some(attributes);
        m.set_owner_username(owner.into());
        m.set_length(length);
        m
    }

    fn rootlist() -> SelectedListContent {
        let mut contents = ListItems::new();
        contents.items = vec![
            item("spotify:playlist:aaaaaaaaaaaaaaaaaaaaaa"),
            item("spotify:start-group:abc:Road%20trips"),
            item("spotify:playlist:bbbbbbbbbbbbbbbbbbbbbb"),
            item("spotify:end-group:abc"),
        ];
        contents.meta_items = vec![
            meta("Mine", "1234567890", 12),
            meta("", "", 0),
            meta("Discover Weekly", "spotify", 30),
            meta("", "", 0),
        ];
        let mut content = SelectedListContent::new();
        content.contents = MessageField::some(contents);
        content
    }

    #[test]
    fn the_rootlist_becomes_playlists_without_folder_markers() {
        let got = summaries(&rootlist());
        assert_eq!(
            got,
            vec![
                PlaylistSummary {
                    uri: "spotify:playlist:aaaaaaaaaaaaaaaaaaaaaa".into(),
                    name: "Mine".into(),
                    owner: "1234567890".into(),
                    covers: vec![],
                    description: String::new(),
                },
                PlaylistSummary {
                    uri: "spotify:playlist:bbbbbbbbbbbbbbbbbbbbbb".into(),
                    name: "Discover Weekly".into(),
                    owner: "spotify".into(),
                    covers: vec![],
                    description: String::new(),
                },
            ]
        );
    }

    /// A list's attributes with the named sizes, and optionally the single
    /// picture a list without an uploaded cover carries.
    fn attributes(sizes: &[(&str, &str)], picture: &[u8]) -> ListAttributes {
        use librespot_protocol::playlist4_external::PictureSize;
        let mut a = ListAttributes::new();
        a.picture_size = sizes
            .iter()
            .map(|(name, url)| {
                let mut p = PictureSize::new();
                p.set_target_name((*name).into());
                p.set_url((*url).into());
                p
            })
            .collect();
        if !picture.is_empty() {
            a.set_picture(picture.to_vec());
        }
        a
    }

    #[test]
    fn playlist_covers_come_in_the_sizes_spotify_names() {
        let covers = playlist_covers(&attributes(
            &[
                ("default", "https://i.scdn.co/image/d"),
                ("large", "https://i.scdn.co/image/l"),
                ("xlarge", "https://i.scdn.co/image/l"),
                ("small", "https://i.scdn.co/image/s"),
                ("default", ""),
            ],
            &[],
        ));
        assert_eq!(
            covers.iter().map(|c| c.width).collect::<Vec<_>>(),
            vec![60, 300, 640]
        );
        assert_eq!(covers[0].url, "https://i.scdn.co/image/s");
    }

    #[test]
    fn an_image_uri_becomes_a_cdn_address() {
        let covers = playlist_covers(&attributes(&[("default", "spotify:image:ab12")], &[]));
        assert_eq!(covers[0].url, "https://i.scdn.co/image/ab12");
    }

    #[test]
    fn a_playlist_spotify_draws_a_mosaic_for_falls_back_to_its_one_picture() {
        let covers = playlist_covers(&attributes(&[], &[0xab, 0x01, 0xff]));
        assert_eq!(
            covers,
            vec![Cover {
                url: "https://i.scdn.co/image/ab01ff".into(),
                width: 640,
            }]
        );
    }

    #[test]
    fn the_sizes_win_from_the_single_picture() {
        let covers = playlist_covers(&attributes(
            &[("default", "https://i.scdn.co/image/d")],
            &[0xab],
        ));
        assert_eq!(covers.len(), 1);
        assert_eq!(covers[0].url, "https://i.scdn.co/image/d");
    }

    #[test]
    fn a_playlist_without_any_picture_has_no_cover() {
        assert!(playlist_covers(&attributes(&[], &[])).is_empty());
    }

    #[test]
    fn a_playlist_page_keeps_only_tracks() {
        let mut contents = ListItems::new();
        contents.items = vec![
            item("spotify:track:1111111111111111111111"),
            item("spotify:episode:2222222222222222222222"),
            item("spotify:local:Artist:Album:Song:180"),
            item("spotify:track:3333333333333333333333"),
        ];
        let mut content = SelectedListContent::new();
        content.contents = MessageField::some(contents);
        assert_eq!(
            track_uris(&content),
            vec![
                "spotify:track:1111111111111111111111".to_string(),
                "spotify:track:3333333333333333333333".to_string(),
            ]
        );
    }

    fn image(file: &[u8], size: Size, width: i32) -> Image {
        let mut i = Image::new();
        i.set_file_id(file.to_vec());
        i.set_size(size);
        if width > 0 {
            i.set_width(width);
        }
        i
    }

    #[test]
    fn covers_point_at_the_image_cdn_and_know_their_width() {
        let c = cover(&image(&[0xab, 0x01, 0xff], Size::DEFAULT, 300)).unwrap();
        assert_eq!(
            c,
            Cover {
                url: "https://i.scdn.co/image/ab01ff".into(),
                width: 300
            }
        );
        // widths missing from the response follow from the size class
        assert_eq!(cover(&image(&[1], Size::SMALL, 0)).unwrap().width, 64);
        assert_eq!(cover(&image(&[1], Size::LARGE, 0)).unwrap().width, 640);
        assert_eq!(cover(&Image::new()), None);
    }

    #[test]
    fn a_track_message_becomes_track_info() {
        let mut artist = Artist::new();
        artist.set_name("Radiohead".into());
        let mut group = ImageGroup::new();
        group.image = vec![image(&[2], Size::LARGE, 640), image(&[1], Size::SMALL, 64)];
        let mut album = Album::new();
        album.set_name("OK Computer".into());
        album.cover_group = MessageField::some(group);
        let mut track = Track::new();
        track.set_name("Airbag".into());
        track.set_duration(284_000);
        track.artist = vec![artist];
        track.album = MessageField::some(album);

        let info = track_info("spotify:track:x", &track);
        assert_eq!(info.uri, "spotify:track:x");
        assert_eq!(info.title, "Airbag");
        assert_eq!(info.artists, vec!["Radiohead".to_string()]);
        assert_eq!(info.album, "OK Computer");
        assert_eq!(info.duration_ms, 284_000);
        assert_eq!(
            info.covers.iter().map(|c| c.width).collect::<Vec<_>>(),
            vec![64, 640]
        );
    }

    #[test]
    fn collection_pages_keep_what_is_still_saved() {
        use crate::collection::CollectionItem;
        let entry = |uri: &str, removed: bool| CollectionItem {
            uri: uri.into(),
            is_removed: removed,
            ..Default::default()
        };
        let mut page = PageResponse {
            items: vec![
                entry("spotify:track:a", false),
                entry("spotify:album:b", false),
                entry("spotify:track:c", true),
                entry("spotify:track:d", false),
            ],
            next_page_token: String::new(),
        };
        page.next_page_token = "t2".into();
        assert_eq!(next_token(&page, "t1"), Some("t2".into()));
        assert_eq!(
            next_token(&page, "t2"),
            None,
            "a repeated token ends the paging"
        );
        page.next_page_token.clear();
        assert_eq!(next_token(&page, "t1"), None);
    }

    #[test]
    fn saved_items_come_newest_first() {
        use crate::collection::CollectionItem;
        let item = |uri: &str, added_at: i32, is_removed: bool| CollectionItem {
            uri: uri.into(),
            added_at,
            is_removed,
        };
        let items = [
            item("spotify:track:old", 1_000, false),
            item("spotify:track:new", 3_000, false),
            item("spotify:album:x", 4_000, false),
            item("spotify:track:gone", 5_000, true),
            item("spotify:track:mid-a", 2_000, false),
            item("spotify:track:mid-b", 2_000, false),
        ];
        assert_eq!(
            newest_first(&items, "spotify:track:"),
            vec![
                "spotify:track:new",
                "spotify:track:mid-a",
                "spotify:track:mid-b",
                "spotify:track:old"
            ]
        );
    }

    fn gid(byte: u8) -> Vec<u8> {
        vec![byte; 16]
    }

    #[test]
    fn album_tracks_and_artist_groups_become_uris() {
        use librespot_protocol::metadata::{Disc, TopTracks};
        let mut t1 = Track::new();
        t1.set_gid(gid(1));
        let mut t2 = Track::new();
        t2.set_gid(gid(2));
        let mut disc = Disc::new();
        disc.track = vec![t1.clone(), t2.clone()];
        let mut album = Album::new();
        album.disc = vec![disc];
        let uris = album_track_uris(&album);
        assert_eq!(uris.len(), 2);
        assert!(uris[0].starts_with("spotify:track:"));
        assert_eq!(uris[0].len(), "spotify:track:".len() + 22);

        let mut nl = TopTracks::new();
        nl.set_country("NL".into());
        nl.track = vec![t2];
        let mut us = TopTracks::new();
        us.set_country("US".into());
        us.track = vec![t1];
        let mut artist = Artist::new();
        artist.top_track = vec![us, nl];
        assert_eq!(top_track_uris(&artist, "NL"), vec![uris[1].clone()]);
        assert_eq!(
            top_track_uris(&artist, "SE"),
            vec![uris[0].clone()],
            "falls back to the first"
        );

        let mut a1 = Album::new();
        a1.set_gid(gid(3));
        let mut a1b = Album::new();
        a1b.set_gid(gid(4));
        let mut group = AlbumGroup::new();
        group.album = vec![a1, a1b];
        assert_eq!(group_uris(&[group]).len(), 1, "one per group");
    }

    #[test]
    fn the_batch_asks_for_each_track_once() {
        let req = batch(
            ExtensionKind::TRACK_V4,
            &["spotify:track:a".into(), "spotify:track:b".into()],
        );
        assert_eq!(req.entity_request.len(), 2);
        assert_eq!(req.entity_request[1].entity_uri, "spotify:track:b");
        assert_eq!(
            req.entity_request[0].query[0].extension_kind,
            EnumOrUnknown::new(ExtensionKind::TRACK_V4)
        );
    }
}
