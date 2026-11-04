//! Searches through pathfinder with the stored spike credentials.
//!
//!   cargo run -p descant-spotify --example search -- radiohead

use std::{env, path::PathBuf, time::Instant};

use descant_spotify::search;
use librespot_core::{cache::Cache, config::SessionConfig, session::Session};

#[tokio::main]
async fn main() {
    let term = env::args().nth(1).unwrap_or_else(|| "radiohead".into());
    let dir = PathBuf::from(env::var_os("HOME").unwrap()).join(".cache/descant-spikes/creds");
    let credentials = Cache::new(Some(dir), None, None, None)
        .unwrap()
        .credentials()
        .unwrap();
    let session = Session::new(SessionConfig::default(), None);
    session.connect(credentials, false).await.unwrap();
    let t = Instant::now();
    let found = search::search(&session, &term, 10).await.expect("search");
    println!(
        "'{term}' in {:?}: {} songs, {} artists, {} albums, {} playlists",
        t.elapsed(),
        found.tracks.len(),
        found.artists.len(),
        found.albums.len(),
        found.playlists.len()
    );
    for track in found.tracks.iter().take(3) {
        println!(
            "  ♪ {} — {} ({} covers)",
            track.title,
            track.artist_line(),
            track.covers.len()
        );
    }
    if let Some(a) = found.artists.first() {
        println!("  artist {} ({} covers)", a.name, a.covers.len());
    }
    if let Some(p) = found.playlists.first() {
        println!("  playlist {} by {}", p.name, p.owner);
    }
}
