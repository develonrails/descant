//! What pages need from the window: the session, a way to play, a way to open
//! other pages, which track is playing, and the library's shared state.

use std::{
    cell::RefCell,
    collections::HashSet,
    rc::Rc,
    time::{Duration, Instant},
};

use descant_core::{supervisor::Command, track::Cover};
use descant_playback::controller::Session;
use descant_spotify::{
    home::{Item, Top},
    library::{Collection, PlaylistSummary},
    profile::Profile,
};

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
    /// Who is signed in, once Preferences asked.
    pub profile: RefCell<Option<Profile>>,
}

#[derive(Default)]
pub struct HomeShelves {
    pub made_for_you: Fresh<Vec<PlaylistSummary>>,
    pub recently_played: Fresh<Vec<Item>>,
    pub top: Fresh<Top>,
}

/// A value and when it was fetched.
pub struct Fresh<T>(RefCell<Option<(Instant, Rc<T>)>>);

impl<T> Default for Fresh<T> {
    fn default() -> Fresh<T> {
        Fresh(RefCell::new(None))
    }
}

impl<T> Fresh<T> {
    /// The value, if it is younger than `max_age`.
    pub fn get(&self, max_age: Duration) -> Option<Rc<T>> {
        let kept = self.0.borrow();
        let (at, value) = kept.as_ref()?;
        (at.elapsed() < max_age).then(|| value.clone())
    }

    pub fn set(&self, value: Rc<T>) {
        *self.0.borrow_mut() = Some((Instant::now(), value));
    }

    pub fn clear(&self) {
        self.0.take();
    }
}

impl Library {
    /// Takes in a fresh read of the collection.
    pub fn set_collection(&self, collection: Collection) {
        *self.liked.borrow_mut() = collection.tracks.iter().cloned().collect();
        *self.collection.borrow_mut() = Some(collection);
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
        self.home.made_for_you.clear();
        self.home.recently_played.clear();
        self.home.top.clear();
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
    fn liking_before_the_collection_is_read_still_marks_the_heart() {
        let library = Library::default();
        library.set_liked("a", true);
        assert!(library.liked.borrow().contains("a"));
        assert!(library.collection.borrow().is_none());
    }
}
