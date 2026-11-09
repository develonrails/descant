//! What pages need from the window: the session, a way to play, a way to open
//! other pages, which track is playing, and the library's shared state.

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
    time::{Duration, Instant},
};

use descant_core::{supervisor::Command, track::Cover};
use descant_playback::controller::Session;
use descant_spotify::{
    home::{Item, Top},
    library::{AlbumSummary, ArtistSummary, Collection, PlaylistSummary},
    podcasts::{Episode, ShowSummary},
    profile::Profile,
};
use gtk::{glib, prelude::*};
use librespot_core::SpotifyId;

/// Where a page can lead.
#[derive(Clone)]
pub enum Destination {
    Liked,
    Album {
        uri: String,
        name: String,
    },
    Artist {
        uri: String,
        name: String,
    },
    Playlist {
        uri: String,
        name: String,
        covers: Vec<Cover>,
    },
    /// A podcast.
    Show {
        uri: String,
        name: String,
    },
}

/// Something pages tell the window.
pub enum Event {
    Toast(String),
    /// The user's playlists changed (one was created): reload the sidebar.
    PlaylistsChanged,
    /// A track was liked or unliked: update hearts.
    LikedChanged,
    /// Forget the account (from Preferences).
    LogOut,
}

/// State shared by every page.
#[derive(Default)]
pub struct Library {
    /// The current session: replaced on every reconnect, so pages opened
    /// before it keep working.
    pub session: RefCell<Option<Session>>,
    /// Liked tracks, for hearts and menus ([`Library::is_liked`]); also the
    /// ones liked here before Liked Songs was read.
    liked: RefCell<HashSet<SpotifyId>>,
    /// Liked Songs newest first, once read. Shared with the Liked Songs page
    /// rather than copied for it; a like made here while a page holds it
    /// gives the library its own copy.
    pub liked_songs: RefCell<Option<Rc<Vec<SpotifyId>>>>,
    /// Saved albums newest first, once read.
    pub saved_albums: RefCell<Option<Vec<String>>>,
    pub playlists: RefCell<Vec<PlaylistSummary>>,
    /// Home's shelves, kept a while so going back to Home costs nothing.
    pub home: HomeShelves,
    /// The library places, kept the same way.
    pub places: LibraryPlaces,
    /// Who is signed in, once Preferences asked.
    pub profile: RefCell<Option<Profile>>,
    /// Followed shows, most recently followed first (Home reads the first
    /// few), and Your Episodes, by URI, for the buttons that change them.
    pub followed_shows: RefCell<Vec<String>>,
    pub saved_episodes: RefCell<HashSet<String>>,
    /// When those two were read, if they were: when podcasts first show, and
    /// again once that is [`SLOW`] ago.
    pub podcasts_read: Cell<Option<Instant>>,
    /// Images waiting for a playlist's cover: one without a picture of its own
    /// borrows its first song's, which the sidebar finds after Home was built.
    pub awaited_covers: RefCell<Vec<(String, glib::WeakRef<gtk::Image>)>>,
}

#[derive(Default)]
pub struct HomeShelves {
    pub made_for_you: Fresh<Vec<PlaylistSummary>>,
    pub recently_played: Fresh<Vec<Item>>,
    pub top: Fresh<Top>,
    /// The newest episodes of followed shows: Continue listening and New episodes.
    pub episodes: Fresh<Vec<Episode>>,
}

/// What the Albums, Artists, Podcasts and Your Episodes places show. A
/// change made here drops the one it touches; changes made elsewhere show
/// once it is [`SLOW`] old.
#[derive(Default)]
pub struct LibraryPlaces {
    pub albums: Fresh<Vec<AlbumSummary>>,
    pub artists: Fresh<Vec<ArtistSummary>>,
    pub shows: Fresh<Vec<ShowSummary>>,
    pub episodes: Fresh<Vec<Episode>>,
}

/// How long kept data is given out: what changes with every track, and the rest.
pub const QUICK: Duration = Duration::from_secs(2 * 60);
pub const SLOW: Duration = Duration::from_secs(30 * 60);

/// Data kept while fresh, otherwise fetched (on the runtime) and kept;
/// `fetch` makes the read only when one is needed.
pub async fn kept<T, F>(
    fresh: &Fresh<T>,
    max_age: Duration,
    fetch: impl FnOnce() -> F,
) -> Option<Rc<T>>
where
    T: Send + 'static,
    F: Future<Output = Result<T, librespot_core::Error>> + Send + 'static,
{
    if let Some(value) = fresh.get(max_age) {
        return Some(value);
    }
    let clears = fresh.clears.get();
    let value = Rc::new(crate::runtime().spawn(fetch()).await.ok()?.ok()?);
    // Cleared meanwhile (a change made while this was read): show it, but
    // do not keep what may predate the change.
    if fresh.clears.get() == clears {
        fresh.set(value.clone());
    }
    Some(value)
}

/// A track URI's id, which is all of it that differs from track to track;
/// `None` for anything but a track.
fn track_id(uri: &str) -> Option<SpotifyId> {
    SpotifyId::from_base62(uri.strip_prefix("spotify:track:")?).ok()
}

/// The URI of the track with id `id`.
pub fn track_uri(id: &SpotifyId) -> String {
    format!("spotify:track:{}", id.to_base62())
}

/// A value and when it was fetched.
pub struct Fresh<T> {
    kept: RefCell<Option<(Instant, Rc<T>)>>,
    /// How often it was cleared, so a read under way at the time is not kept.
    clears: Cell<u64>,
}

impl<T> Default for Fresh<T> {
    fn default() -> Fresh<T> {
        Fresh {
            kept: RefCell::new(None),
            clears: Cell::new(0),
        }
    }
}

impl<T> Fresh<T> {
    /// The value, if it is younger than `max_age`.
    pub fn get(&self, max_age: Duration) -> Option<Rc<T>> {
        let kept = self.kept.borrow();
        let (at, value) = kept.as_ref()?;
        (at.elapsed() < max_age).then(|| value.clone())
    }

    pub fn set(&self, value: Rc<T>) {
        *self.kept.borrow_mut() = Some((Instant::now(), value));
    }

    pub fn clear(&self) {
        self.kept.take();
        self.clears.set(self.clears.get() + 1);
    }
}

impl Library {
    /// Fills `image` once a cover for the playlist `uri` is found.
    pub fn await_cover(&self, uri: &str, image: &gtk::Image) {
        self.awaited_covers
            .borrow_mut()
            .push((uri.to_string(), image.downgrade()));
    }

    /// The images waiting for `uri`'s cover. They stop waiting; so do the
    /// ones whose page is gone.
    pub fn take_awaiting(&self, uri: &str) -> Vec<gtk::Image> {
        let mut found = Vec::new();
        self.awaited_covers
            .borrow_mut()
            .retain(|(waits_for, weak)| match weak.upgrade() {
                Some(image) if waits_for == uri => {
                    found.push(image);
                    false
                }
                Some(_) => true,
                None => false,
            });
        found
    }

    /// Takes in a fresh read of the collection. Saved albums that changed
    /// meanwhile (elsewhere) make the Albums place read them again.
    pub fn set_collection(&self, collection: Collection) {
        let songs: Vec<SpotifyId> = collection
            .tracks
            .iter()
            .filter_map(|u| track_id(u))
            .collect();
        *self.liked.borrow_mut() = songs.iter().copied().collect();
        *self.liked_songs.borrow_mut() = Some(Rc::new(songs));
        let mut albums = self.saved_albums.borrow_mut();
        if albums.as_ref().is_some_and(|a| *a != collection.albums) {
            self.places.albums.clear();
        }
        *albums = Some(collection.albums);
    }

    /// Whether the track `uri` is in Liked Songs, as far as is known.
    pub fn is_liked(&self, uri: &str) -> bool {
        track_id(uri).is_some_and(|id| self.liked.borrow().contains(&id))
    }

    /// Records a like or unlike made here.
    pub fn set_liked(&self, uri: &str, liked: bool) {
        let Some(id) = track_id(uri) else { return };
        if liked {
            self.liked.borrow_mut().insert(id);
        } else {
            self.liked.borrow_mut().remove(&id);
        }
        if let Some(songs) = &mut *self.liked_songs.borrow_mut() {
            let songs = Rc::make_mut(songs);
            songs.retain(|s| *s != id);
            if liked {
                songs.insert(0, id);
            }
        }
    }

    /// Records a follow or unfollow: the show goes on top, or leaves; the
    /// Podcasts place and Home's episodes are read again.
    pub fn set_followed(&self, uri: &str, followed: bool) {
        let mut shows = self.followed_shows.borrow_mut();
        shows.retain(|u| u != uri);
        if followed {
            shows.insert(0, uri.to_string());
        }
        self.places.shows.clear();
        self.home.episodes.clear();
    }

    /// Whether the show `uri` is followed, as far as is known.
    pub fn follows(&self, uri: &str) -> bool {
        self.followed_shows.borrow().iter().any(|u| u == uri)
    }

    /// Forgets everything, for logging out.
    pub fn clear(&self) {
        self.session.take();
        self.liked.take();
        self.liked_songs.take();
        self.saved_albums.take();
        self.playlists.take();
        self.profile.take();
        self.followed_shows.take();
        self.saved_episodes.take();
        self.podcasts_read.set(None);
        self.awaited_covers.take();
        self.home.made_for_you.clear();
        self.home.recently_played.clear();
        self.home.top.clear();
        self.home.episodes.clear();
        self.places.albums.clear();
        self.places.artists.clear();
        self.places.shows.clear();
        self.places.episodes.clear();
    }
}

/// Holds no session of its own: pages outlive reconnects, and one kept here
/// would keep every replaced session alive with the page.
#[derive(Clone)]
pub struct Nav {
    send: Rc<dyn Fn(Command)>,
    open: Rc<dyn Fn(Destination)>,
    event: Rc<dyn Fn(Event)>,
    /// The URI of the track that plays now, shared by every page.
    pub current: Rc<RefCell<String>>,
    pub library: Rc<Library>,
}

impl Nav {
    pub fn new(
        send: Rc<dyn Fn(Command)>,
        open: Rc<dyn Fn(Destination)>,
        event: Rc<dyn Fn(Event)>,
        current: Rc<RefCell<String>>,
        library: Rc<Library>,
    ) -> Nav {
        Nav {
            send,
            open,
            event,
            current,
            library,
        }
    }

    /// The current session (the newest after a reconnect); none once logged
    /// out, when whatever a page was still doing has no one to do it for.
    pub fn session(&self) -> Option<Session> {
        self.library.session.borrow().clone()
    }

    /// Reads which shows are followed and which episodes saved, the first
    /// time a page with podcasts needs it (and again once that is a while
    /// ago); listeners who never open one never pay for it.
    pub async fn podcast_state(&self) {
        let Some(session) = self.session() else {
            return;
        };
        let read_at = self.library.podcasts_read.get();
        if read_at.is_some_and(|at| at.elapsed() < SLOW) {
            return;
        }
        // Marked before reading, so pages opened meanwhile do not read too.
        self.library.podcasts_read.set(Some(Instant::now()));
        let read = crate::runtime()
            .spawn(async move {
                use descant_spotify::podcasts::saved_uris;
                (
                    saved_uris(&session, "show").await,
                    saved_uris(&session, "listenlater").await,
                )
            })
            .await;
        match read {
            Ok((Ok(shows), Ok(episodes))) => {
                *self.library.followed_shows.borrow_mut() = shows;
                *self.library.saved_episodes.borrow_mut() = episodes.into_iter().collect();
            }
            _ => self.library.podcasts_read.set(read_at), // try again next time
        }
    }

    /// Follows or unfollows a show.
    pub async fn set_followed(&self, uri: String, followed: bool) -> bool {
        let Some(session) = self.session() else {
            return false;
        };
        let u = uri.clone();
        let done = crate::runtime()
            .spawn(async move { descant_spotify::edit::set_followed(&session, &u, followed).await })
            .await;
        let ok = matches!(done, Ok(Ok(())));
        if ok {
            self.library.set_followed(&uri, followed);
        }
        self.toast(match (ok, followed) {
            (true, true) => "Following",
            (true, false) => "No longer following",
            (false, _) => "Spotify did not take that change",
        });
        ok
    }

    /// Puts an episode in Your Episodes or takes it out.
    pub async fn set_saved_episode(&self, uri: String, saved: bool) -> bool {
        let Some(session) = self.session() else {
            return false;
        };
        let u = uri.clone();
        let done = crate::runtime()
            .spawn(
                async move { descant_spotify::edit::set_saved_episode(&session, &u, saved).await },
            )
            .await;
        let ok = matches!(done, Ok(Ok(())));
        if ok {
            self.library.places.episodes.clear();
            let mut episodes = self.library.saved_episodes.borrow_mut();
            if saved {
                episodes.insert(uri);
            } else {
                episodes.remove(&uri);
            }
        }
        self.toast(match (ok, saved) {
            (true, true) => "Saved to Your Episodes",
            (true, false) => "Removed from Your Episodes",
            (false, _) => "Spotify did not take that change",
        });
        ok
    }

    /// Likes or unlikes a track, and tells everyone.
    pub async fn set_liked(&self, uri: String, liked: bool) -> bool {
        let Some(session) = self.session() else {
            return false;
        };
        let u = uri.clone();
        let done = crate::runtime()
            .spawn(async move { descant_spotify::edit::set_liked(&session, &u, liked).await })
            .await;
        if matches!(done, Ok(Ok(()))) {
            self.library.set_liked(&uri, liked);
            self.toast(if liked {
                "Saved to Liked Songs"
            } else {
                "Removed from Liked Songs"
            });
            self.tell(Event::LikedChanged);
            true
        } else {
            self.toast("Spotify did not take that change");
            false
        }
    }

    pub fn send(&self, command: Command) {
        (self.send)(command)
    }

    pub fn open(&self, destination: Destination) {
        (self.open)(destination)
    }

    pub fn tell(&self, event: Event) {
        (self.event)(event)
    }

    pub fn toast(&self, message: impl Into<String>) {
        self.tell(Event::Toast(message.into()));
    }

    /// Plays `context`, optionally starting at `track`.
    pub fn play(&self, context: &str, track: Option<&str>) {
        self.send(Command::Load {
            context: context.to_string(),
            start_track: track.map(str::to_string),
        });
    }

    /// The user's own playlists (which they can add to): uri and name.
    pub fn own_playlists(&self) -> Vec<(String, String)> {
        let Some(me) = self.session().map(|s| s.username()) else {
            return Vec::new();
        };
        self.library
            .playlists
            .borrow()
            .iter()
            .filter(|p| p.owner == me)
            .map(|p| (p.uri.clone(), p.name.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A track URI, told apart by its last character.
    fn song(last: char) -> String {
        format!("spotify:track:000000000000000000000{last}")
    }

    fn library(tracks: &[char]) -> Library {
        let library = Library::default();
        library.set_collection(Collection {
            tracks: tracks.iter().map(|t| song(*t)).collect(),
            albums: vec![],
        });
        library
    }

    fn order(library: &Library) -> Vec<String> {
        let songs = library.liked_songs.borrow().clone().unwrap();
        songs.iter().map(track_uri).collect()
    }

    #[test]
    fn a_new_like_goes_on_top() {
        let library = library(&['b', 'c']);
        library.set_liked(&song('a'), true);
        assert_eq!(order(&library), [song('a'), song('b'), song('c')]);
        assert!(library.is_liked(&song('a')));
    }

    #[test]
    fn an_unlike_leaves_the_list_and_the_hearts() {
        let library = library(&['a', 'b']);
        library.set_liked(&song('a'), false);
        assert_eq!(order(&library), [song('b')]);
        assert!(!library.is_liked(&song('a')));
        assert!(library.is_liked(&song('b')));
    }

    #[test]
    fn liked_songs_are_shared_and_a_page_keeps_what_it_was_given() {
        let library = library(&['a', 'b']);
        let shown = library.liked_songs.borrow().clone().unwrap();
        let again = library.liked_songs.borrow().clone().unwrap();
        assert!(Rc::ptr_eq(&shown, &again), "not copied per visit");
        library.set_liked(&song('c'), true);
        assert_eq!(shown.len(), 2, "the open page's list stays as it was");
        assert_eq!(order(&library), [song('c'), song('a'), song('b')]);
    }

    #[test]
    fn track_ids_round_trip_and_only_tracks_are_liked() {
        let uri = "spotify:track:6rqhFgbbKwnb9MLmUQDhG6";
        assert_eq!(track_uri(&track_id(uri).unwrap()), uri);
        let library = library(&[]);
        library.set_liked("spotify:episode:6rqhFgbbKwnb9MLmUQDhG6", true);
        assert!(!library.is_liked("spotify:episode:6rqhFgbbKwnb9MLmUQDhG6"));
        assert!(!library.is_liked(uri));
        assert!(order(&library).is_empty());
    }

    #[test]
    fn a_kept_value_is_given_out_while_fresh() {
        let fresh = Fresh::default();
        assert!(fresh.get(Duration::from_secs(60)).is_none());
        fresh.set(Rc::new(7));
        assert_eq!(fresh.get(Duration::from_secs(60)).as_deref(), Some(&7));
        assert!(fresh.get(Duration::ZERO).is_none());
        fresh.clear();
        assert!(fresh.get(Duration::from_secs(60)).is_none());
    }

    #[test]
    fn saved_albums_changed_elsewhere_drop_the_kept_albums() {
        let library = library(&[]);
        library.places.albums.set(Rc::new(vec![]));
        library.set_collection(Collection::default());
        assert!(library.places.albums.get(SLOW).is_some(), "the same: kept");
        library.set_collection(Collection {
            tracks: vec![],
            albums: vec!["spotify:album:a".into()],
        });
        assert!(library.places.albums.get(SLOW).is_none());
    }

    #[test]
    fn a_new_follow_goes_on_top_and_drops_what_shows_follows() {
        let library = Library::default();
        *library.followed_shows.borrow_mut() = vec!["b".into(), "c".into()];
        library.places.shows.set(Rc::new(vec![]));
        library.home.episodes.set(Rc::new(vec![]));
        library.set_followed("a", true);
        assert_eq!(*library.followed_shows.borrow(), ["a", "b", "c"]);
        assert!(library.follows("a"));
        assert!(library.places.shows.get(SLOW).is_none());
        assert!(library.home.episodes.get(SLOW).is_none());
        library.set_followed("b", false);
        assert_eq!(*library.followed_shows.borrow(), ["a", "c"]);
        assert!(!library.follows("b"));
    }

    #[test]
    fn logging_out_forgets_the_kept_places() {
        let library = Library::default();
        library.places.shows.set(Rc::new(vec![]));
        library.places.episodes.set(Rc::new(vec![]));
        library.clear();
        assert!(library.places.shows.get(SLOW).is_none());
        assert!(library.places.episodes.get(SLOW).is_none());
    }

    #[test]
    fn liking_before_the_collection_is_read_still_marks_the_heart() {
        let library = Library::default();
        library.set_liked(&song('a'), true);
        assert!(library.is_liked(&song('a')));
        assert!(library.liked_songs.borrow().is_none());
    }
}
