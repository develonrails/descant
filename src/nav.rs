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
    /// Liked tracks, for hearts and menus.
    pub liked: RefCell<HashSet<String>>,
    /// Liked Songs and saved albums in order, once read.
    pub collection: RefCell<Option<Collection>>,
    pub playlists: RefCell<Vec<PlaylistSummary>>,
    /// Home's shelves, kept a while so going back to Home costs nothing.
    pub home: HomeShelves,
    /// The library places, kept the same way.
    pub places: LibraryPlaces,
    /// Who is signed in, once Preferences asked.
    pub profile: RefCell<Option<Profile>>,
    /// Followed shows and Your Episodes, by URI, for the buttons that change them.
    pub followed_shows: RefCell<HashSet<String>>,
    pub saved_episodes: RefCell<HashSet<String>>,
    /// Whether those two were read: once per login, when podcasts first show.
    pub podcasts_read: std::cell::Cell<bool>,
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
        *self.liked.borrow_mut() = collection.tracks.iter().cloned().collect();
        let mut kept = self.collection.borrow_mut();
        if kept.as_ref().is_some_and(|k| k.albums != collection.albums) {
            self.places.albums.clear();
        }
        *kept = Some(collection);
    }

    /// Records a like or unlike made here.
    pub fn set_liked(&self, uri: &str, liked: bool) {
        if liked {
            self.liked.borrow_mut().insert(uri.to_string());
        } else {
            self.liked.borrow_mut().remove(uri);
        }
        if let Some(collection) = &mut *self.collection.borrow_mut() {
            collection.tracks.retain(|u| u != uri);
            if liked {
                collection.tracks.insert(0, uri.to_string());
            }
        }
    }

    /// Forgets everything, for logging out.
    pub fn clear(&self) {
        self.session.take();
        self.liked.take();
        self.collection.take();
        self.playlists.take();
        self.profile.take();
        self.followed_shows.take();
        self.saved_episodes.take();
        self.podcasts_read.set(false);
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

#[derive(Clone)]
pub struct Nav {
    /// The session when the page was built; used only if the shared one is gone.
    initial: Session,
    send: Rc<dyn Fn(Command)>,
    open: Rc<dyn Fn(Destination)>,
    event: Rc<dyn Fn(Event)>,
    /// The URI of the track that plays now, shared by every page.
    pub current: Rc<RefCell<String>>,
    pub library: Rc<Library>,
}

impl Nav {
    pub fn new(
        session: Session,
        send: Rc<dyn Fn(Command)>,
        open: Rc<dyn Fn(Destination)>,
        event: Rc<dyn Fn(Event)>,
        current: Rc<RefCell<String>>,
        library: Rc<Library>,
    ) -> Nav {
        Nav {
            initial: session,
            send,
            open,
            event,
            current,
            library,
        }
    }

    /// The current session (the newest after a reconnect).
    pub fn session(&self) -> Session {
        self.library
            .session
            .borrow()
            .clone()
            .unwrap_or_else(|| self.initial.clone())
    }

    /// Reads which shows are followed and which episodes saved, the first
    /// time a page with podcasts needs it; listeners who never open one never
    /// pay for it.
    pub async fn podcast_state(&self) {
        if self.library.podcasts_read.replace(true) {
            return;
        }
        let session = self.session();
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
                *self.library.followed_shows.borrow_mut() = shows.into_iter().collect();
                *self.library.saved_episodes.borrow_mut() = episodes.into_iter().collect();
            }
            _ => self.library.podcasts_read.set(false), // try again next time
        }
    }

    /// Follows or unfollows a show.
    pub async fn set_followed(&self, uri: String, followed: bool) -> bool {
        let session = self.session();
        let u = uri.clone();
        let done = crate::runtime()
            .spawn(async move { descant_spotify::edit::set_followed(&session, &u, followed).await })
            .await;
        let ok = matches!(done, Ok(Ok(())));
        if ok {
            self.library.places.shows.clear();
            let mut shows = self.library.followed_shows.borrow_mut();
            if followed {
                shows.insert(uri);
            } else {
                shows.remove(&uri);
            }
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
        let session = self.session();
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
        let session = self.session();
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
        let me = self.session().username();
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

    fn library(tracks: &[&str]) -> Library {
        let library = Library::default();
        library.set_collection(Collection {
            tracks: tracks.iter().map(|t| t.to_string()).collect(),
            albums: vec![],
        });
        library
    }

    fn order(library: &Library) -> Vec<String> {
        library.collection.borrow().as_ref().unwrap().tracks.clone()
    }

    #[test]
    fn a_new_like_goes_on_top() {
        let library = library(&["b", "c"]);
        library.set_liked("a", true);
        assert_eq!(order(&library), ["a", "b", "c"]);
        assert!(library.liked.borrow().contains("a"));
    }

    #[test]
    fn an_unlike_leaves_the_list_and_the_hearts() {
        let library = library(&["a", "b"]);
        library.set_liked("a", false);
        assert_eq!(order(&library), ["b"]);
        assert!(!library.liked.borrow().contains("a"));
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
        library.set_liked("a", true);
        assert!(library.liked.borrow().contains("a"));
        assert!(library.collection.borrow().is_none());
    }
}
