//! Loads what Home shows, with the stored spike credentials.
use std::{env, path::PathBuf, time::Instant};

use descant_spotify::home::{self, Item};
use librespot_core::{cache::Cache, config::SessionConfig, session::Session};

#[tokio::main]
async fn main() {
    let dir = PathBuf::from(env::var_os("HOME").unwrap()).join(".cache/descant-spikes/creds");
    let credentials = Cache::new(Some(dir), None, None, None)
        .unwrap()
        .credentials()
        .unwrap();
    let session = Session::new(SessionConfig::default(), None);
    session.connect(credentials, false).await.unwrap();

    let t = Instant::now();
    let mfy = home::made_for_you(&session).await.expect("made for you");
    println!(
        "Made for you: {} in {:?}: {:?}",
        mfy.len(),
        t.elapsed(),
        mfy.iter()
            .take(3)
            .map(|p| (&p.name, &p.description))
            .collect::<Vec<_>>()
    );
    let t = Instant::now();
    let top = home::top(&session, 10).await.expect("top");
    println!(
        "Top: {} artists, {} tracks in {:?}; {:?} / {:?}",
        top.artists.len(),
        top.tracks.len(),
        t.elapsed(),
        top.artists
            .iter()
            .take(3)
            .map(|a| &a.name)
            .collect::<Vec<_>>(),
        top.tracks
            .iter()
            .take(3)
            .map(|t| &t.title)
            .collect::<Vec<_>>()
    );
    let t = Instant::now();
    let recent = home::recently_played(&session, 20).await.expect("recent");
    println!("Recently played: {} in {:?}", recent.len(), t.elapsed());
    for item in recent.iter().take(6) {
        match item {
            Item::Playlist(p) => println!("  playlist {} ({} covers)", p.name, p.covers.len()),
            Item::Album(a) => println!("  album {} — {}", a.name, a.artists.join(", ")),
            Item::Artist(a) => println!("  artist {}", a.name),
        }
    }
}
