//! Changing the library: liking tracks, and editing playlists.
//!
//! Writes go straight to spclient with the session's login5 and client
//! tokens, **sent once**: librespot's spclient retries failed requests, and a
//! playlist change resent after Spotify applied it (but its answer was lost)
//! would be applied twice. This is what librespot PR #1831 proposes too.
//!
//! Playlist changes are based on the revision they were made against; a
//! playlist changed meanwhile answers 409 (`Aborted`), and nothing is applied
//! on a guess. Message shapes follow spotatui and Spotifast (both MIT).

use std::time::{SystemTime, UNIX_EPOCH};

use librespot_core::{Error, SpotifyId, session::Session};
use librespot_protocol::playlist4_external::{
    Add, ChangeInfo, Delta, Item, ItemAttributes, ListAttributes, ListAttributesPartialState,
    ListChanges, Op, Rem, SelectedListContent, UpdateListAttributes, op::Kind,
};
use protobuf::{Message, MessageField};

use crate::{
    collection::write_request,
    library::PlaylistSummary,
    net::{self, COLLECTION_TYPE, playlist_id},
};

/// The revision a brand-new playlist starts from.
const NEW_PLAYLIST_REVISION: &[u8] = b"\0\0\0\0root";
const PROTOBUF_TYPE: &str = "application/x-protobuf";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---- messages (pure) -----------------------------------------------------------

pub fn item(uri: &str, timestamp_ms: Option<i64>, public: Option<bool>) -> Item {
    let mut item = Item::new();
    item.set_uri(uri.to_string());
    let mut attributes = ItemAttributes::new();
    if let Some(ts) = timestamp_ms {
        attributes.set_timestamp(ts);
    }
    if let Some(public) = public {
        attributes.set_public(public);
    }
    item.attributes = MessageField::some(attributes);
    item
}

/// Adds `items` at `index`, or at the end.
pub fn add_op(items: Vec<Item>, index: Option<i32>) -> Op {
    let mut add = Add::new();
    add.items = items;
    match index {
        Some(index) => add.set_from_index(index),
        None => add.set_add_last(true),
    }
    let mut op = Op::new();
    op.set_kind(Kind::ADD);
    op.add = MessageField::some(add);
    op
}

/// Removes the one row at `index`, which must hold `uri`.
pub fn remove_op(index: i32, uri: &str) -> Op {
    let mut rem = Rem::new();
    rem.set_from_index(index);
    rem.set_length(1);
    rem.items.push(item(uri, None, None));
    let mut op = Op::new();
    op.set_kind(Kind::REM);
    op.rem = MessageField::some(rem);
    op
}

pub fn rename_op(name: &str) -> Op {
    let mut values = ListAttributes::new();
    values.set_name(name.to_string());
    let mut op = Op::new();
    op.set_kind(Kind::UPDATE_LIST_ATTRIBUTES);
    op.update_list_attributes = MessageField::some(UpdateListAttributes {
        new_attributes: MessageField::some(ListAttributesPartialState {
            values: MessageField::some(values),
            ..Default::default()
        }),
        ..Default::default()
    });
    op
}

pub fn list_changes(
    user: &str,
    timestamp_ms: i64,
    base_revision: &[u8],
    ops: Vec<Op>,
) -> ListChanges {
    let mut info = ChangeInfo::new();
    info.set_user(user.to_string());
    info.set_timestamp(timestamp_ms);
    let mut changes = ListChanges::new();
    changes.set_base_revision(base_revision.to_vec());
    changes.deltas.push(Delta {
        ops,
        info: MessageField::some(info),
        ..Default::default()
    });
    changes.set_want_resulting_revisions(true);
    changes.set_want_sync_result(true);
    changes
}

/// Where the `occurrence`-th copy (from 0) of `uri` sits in a playlist's rows.
pub fn row_index(rows: &[String], uri: &str, occurrence: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, r)| *r == uri)
        .nth(occurrence)
        .map(|(i, _)| i)
}

// ---- transport -----------------------------------------------------------------

/// POSTs once to spclient; 409 (the list changed) becomes `Aborted`.
async fn post_once(
    session: &Session,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Result<Vec<u8>, Error> {
    let base = session.spclient().base_url().await?;
    let response = net::authed(session, reqwest::Method::POST, &format!("{base}{path}"))
        .await?
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .header(reqwest::header::ACCEPT, content_type)
        .body(body)
        .send()
        .await
        .map_err(|e| Error::unavailable(e.without_url().to_string()))?;
    match response.status() {
        s if s.is_success() => Ok(response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .unwrap_or_default()),
        reqwest::StatusCode::CONFLICT => Err(Error::aborted("the playlist changed meanwhile")),
        s => Err(Error::unavailable(format!("Spotify answered {s}"))),
    }
}

/// A playlist's current revision and all its rows' URIs, read from one revision.
async fn rows(session: &Session, id: &str) -> Result<(Vec<u8>, Vec<String>), Error> {
    let (mut revision, mut rows) = (None::<Vec<u8>>, Vec::new());
    loop {
        let endpoint = format!(
            "/playlist/v2/playlist/{id}?decorate=revision,length&from={}&length=1000",
            rows.len()
        );
        let bytes = session
            .spclient()
            .request(&http::Method::GET, &endpoint, None, None)
            .await?;
        let content = SelectedListContent::parse_from_bytes(&bytes)?;
        match &revision {
            None => revision = Some(content.revision().to_vec()),
            Some(first) if first.as_slice() != content.revision() => {
                return Err(Error::aborted("the playlist changed meanwhile"));
            }
            _ => {}
        }
        let page: Vec<String> = content
            .contents
            .as_ref()
            .map(|c| c.items.iter().map(|i| i.uri().to_string()).collect())
            .unwrap_or_default();
        let total = content.length().max(0) as usize;
        let empty = page.is_empty();
        rows.extend(page);
        if empty || rows.len() >= total {
            break;
        }
    }
    Ok((revision.unwrap_or_default(), rows))
}

// ---- operations ----------------------------------------------------------------

/// Saves a track to, or removes it from, Liked Songs.
pub async fn set_liked(session: &Session, uri: &str, liked: bool) -> Result<(), Error> {
    let body = write_request(
        &session.username(),
        "collection",
        uri,
        liked,
        now_ms() / 1000,
    );
    post_once(session, "/collection/v2/write", COLLECTION_TYPE, body)
        .await
        .map(|_| ())
}

/// Appends tracks to a playlist.
pub async fn add_to_playlist(
    session: &Session,
    playlist: &str,
    tracks: &[String],
) -> Result<(), Error> {
    let id = playlist_id(playlist)?;
    let endpoint = format!("/playlist/v2/playlist/{id}?decorate=revision&length=0");
    let bytes = session
        .spclient()
        .request(&http::Method::GET, &endpoint, None, None)
        .await?;
    let revision = SelectedListContent::parse_from_bytes(&bytes)?
        .revision()
        .to_vec();
    let now = now_ms();
    let items = tracks.iter().map(|t| item(t, Some(now), None)).collect();
    let changes = list_changes(
        &session.username(),
        now,
        &revision,
        vec![add_op(items, None)],
    );
    let path = format!("/playlist/v2/playlist/{id}/changes");
    post_once(session, &path, PROTOBUF_TYPE, changes.write_to_bytes()?)
        .await
        .map(|_| ())
}

/// Removes the `occurrence`-th copy (from 0) of `track` from a playlist.
pub async fn remove_from_playlist(
    session: &Session,
    playlist: &str,
    track: &str,
    occurrence: usize,
) -> Result<(), Error> {
    let id = playlist_id(playlist)?;
    let (revision, rows) = rows(session, id).await?;
    let index = row_index(&rows, track, occurrence)
        .ok_or_else(|| Error::aborted("the track is no longer in the playlist"))?;
    let changes = list_changes(
        &session.username(),
        now_ms(),
        &revision,
        vec![remove_op(index as i32, track)],
    );
    let path = format!("/playlist/v2/playlist/{id}/changes");
    post_once(session, &path, PROTOBUF_TYPE, changes.write_to_bytes()?)
        .await
        .map(|_| ())
}

/// Creates an empty playlist called `name` at the top of the user's list.
pub async fn create_playlist(session: &Session, name: &str) -> Result<PlaylistSummary, Error> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|e| Error::internal(e.to_string()))?;
    let id = SpotifyId::from_raw(&raw)
        .map_err(|_| Error::internal("id"))?
        .to_base62();
    let user = session.username();
    let changes = list_changes(
        &user,
        now_ms(),
        NEW_PLAYLIST_REVISION,
        vec![rename_op(name)],
    );
    post_once(
        session,
        &format!("/playlist/v2/playlist/{id}/changes"),
        PROTOBUF_TYPE,
        changes.write_to_bytes()?,
    )
    .await?;

    let uri = format!("spotify:playlist:{id}");
    let root = session.spclient().get_rootlist(0, Some(1)).await?;
    let revision = SelectedListContent::parse_from_bytes(&root)?
        .revision()
        .to_vec();
    let add = add_op(vec![item(&uri, Some(now_ms()), Some(true))], Some(0));
    let changes = list_changes(&user, now_ms(), &revision, vec![add]);
    post_once(
        session,
        &format!("/playlist/v2/user/{user}/rootlist/changes"),
        PROTOBUF_TYPE,
        changes.write_to_bytes()?,
    )
    .await?;
    Ok(PlaylistSummary {
        uri,
        name: name.to_string(),
        owner: user,
        covers: vec![],
        description: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_add_appends_with_a_timestamp() {
        let op = add_op(vec![item("spotify:track:a", Some(42), None)], None);
        assert_eq!(op.kind(), Kind::ADD);
        assert!(op.add.add_last());
        assert_eq!(op.add.items[0].uri(), "spotify:track:a");
        assert_eq!(op.add.items[0].attributes.timestamp(), 42);
    }

    #[test]
    fn a_remove_takes_one_row_at_its_index() {
        let op = remove_op(7, "spotify:track:b");
        assert_eq!(op.kind(), Kind::REM);
        assert_eq!((op.rem.from_index(), op.rem.length()), (7, 1));
        assert_eq!(op.rem.items[0].uri(), "spotify:track:b");
    }

    #[test]
    fn changes_carry_their_base_revision_and_author() {
        let changes = list_changes("me", 1_700, b"rev", vec![rename_op("Road Trip")]);
        let decoded = ListChanges::parse_from_bytes(&changes.write_to_bytes().unwrap()).unwrap();
        assert_eq!(decoded.base_revision(), b"rev");
        assert_eq!(decoded.deltas[0].info.user(), "me");
        assert_eq!(decoded.deltas[0].info.timestamp(), 1_700);
        let op = &decoded.deltas[0].ops[0];
        assert_eq!(op.kind(), Kind::UPDATE_LIST_ATTRIBUTES);
        assert_eq!(
            op.update_list_attributes.new_attributes.values.name(),
            "Road Trip"
        );
    }

    #[test]
    fn the_right_copy_of_a_repeated_track_is_found() {
        let rows: Vec<String> = ["a", "b", "a", "c", "a"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(row_index(&rows, "a", 0), Some(0));
        assert_eq!(row_index(&rows, "a", 2), Some(4));
        assert_eq!(row_index(&rows, "a", 3), None);
        assert_eq!(row_index(&rows, "z", 0), None);
    }
}
