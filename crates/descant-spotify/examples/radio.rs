//! Finds the song radio for a track, logged in with the app's stored
//! credentials, and lists a few of its songs.
//!
//!   cargo run -p descant-spotify --example radio -- spotify:track:6rqhFgbbKwnb9MLmUQDhG6

use std::env;

use descant_spotify::{library, radio, store};
use librespot_core::{config::SessionConfig, session::Session};

#[tokio::main]
async fn main() {
    let track = env::args()
        .nth(1)
        .unwrap_or_else(|| "spotify:track:6rqhFgbbKwnb9MLmUQDhG6".into());
    let credentials = store::load()
        .await
        .expect("keyring")
        .expect("log in to Descant first");
    let session = Session::new(SessionConfig::default(), None);
    session.connect(credentials, false).await.unwrap();
    let playlist = radio::for_track(&session, &track).await.expect("radio");
    println!("radio for {track}: {playlist}");
    let page = library::playlist(&session, &playlist, 0, 5)
        .await
        .expect("playlist");
    println!("{} by {}, {} songs", page.name, page.owner, page.total);
}
