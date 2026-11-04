//! Reads the account's playlists and the first page of one, through the
//! session. Uses the credentials stored by spikes/s1-playback.
//!
//!   cargo run -p descant-spotify --example library [PLAYLIST_NUMBER]

use std::{env, path::PathBuf, time::Instant};

use descant_spotify::library;
use librespot_core::{cache::Cache, config::SessionConfig, session::Session};

#[tokio::main]
async fn main() {
    let dir = env::var_os("DESCANT_SPIKE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap()).join(".cache/descant-spikes/creds")
        });
    let credentials = Cache::new(Some(dir), None, None, None)
        .ok()
        .and_then(|c| c.credentials())
        .expect("run spikes/s1-playback login first");
    let session = Session::new(SessionConfig::default(), None);
    session.connect(credentials, false).await.expect("connect");

    let t = Instant::now();
    let playlists = library::playlists(&session).await.expect("playlists");
    println!("{} playlists in {:?}", playlists.len(), t.elapsed());
    for (i, p) in playlists.iter().take(8).enumerate() {
        println!("  {i:>2}. {} (by {})", p.name, p.owner);
    }
    let pick: usize = env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(0);
    if let Some(p) = playlists.get(pick) {
        let t = Instant::now();
        let page = library::playlist(&session, &p.uri, 0, 50)
            .await
            .expect("page");
        println!(
            "\n'{}' by {}: {} rows, first page of {} described in {:?}",
            page.name,
            page.owner,
            page.total,
            page.tracks.len(),
            t.elapsed()
        );
        for track in page.tracks.iter().take(5) {
            println!(
                "  {} — {} [{}] {}:{:02} covers {:?}",
                track.title,
                track.artist_line(),
                track.album,
                track.duration_ms / 60000,
                track.duration_ms / 1000 % 60,
                track.covers.iter().map(|c| c.width).collect::<Vec<_>>()
            );
        }
    }

    let t = Instant::now();
    match library::collection(&session).await {
        Ok(collection) => {
            let uris = collection.tracks;
            let newest = library::tracks(&session, &uris[..uris.len().min(5)])
                .await
                .unwrap_or_default();
            println!(
                "\nLiked Songs: {} in {:?}; newest: {:?}",
                uris.len(),
                t.elapsed(),
                newest.iter().map(|t| &t.title).collect::<Vec<_>>()
            );
        }
        Err(e) => println!("\nLiked Songs: ERROR {e}"),
    }
    let t = Instant::now();
    let album_uris = library::collection(&session)
        .await
        .map(|c| c.albums)
        .unwrap_or_default();
    match library::albums(&session, &album_uris).await {
        Ok(albums) => {
            println!(
                "Saved albums: {} in {:?}; first: {:?}",
                albums.len(),
                t.elapsed(),
                albums
                    .first()
                    .map(|a| (&a.name, &a.artists, a.year, a.covers.len()))
            );
            if let Some(a) = albums.first() {
                let t = Instant::now();
                match library::album(&session, &a.uri).await {
                    Ok(page) => println!(
                        "  album page '{}': {} tracks in {:?}",
                        page.album.name,
                        page.tracks.len(),
                        t.elapsed()
                    ),
                    Err(e) => println!("  album page ERROR {e}"),
                }
            }
        }
        Err(e) => println!("Saved albums: ERROR {e}"),
    }
    let t = Instant::now();
    match library::followed_artists(&session).await {
        Ok(artists) => {
            println!(
                "Followed artists: {} in {:?}; first: {:?}",
                artists.len(),
                t.elapsed(),
                artists.first().map(|a| (&a.name, a.covers.len()))
            );
            let uri = artists
                .first()
                .map(|a| a.uri.clone())
                .unwrap_or("spotify:artist:4Z8W4fKeB5YxbusRsdQVPb".into());
            let t = Instant::now();
            match library::artist(&session, &uri, 24).await {
                Ok(page) => println!(
                    "  artist page '{}': {} top tracks, {} albums, {} singles in {:?}",
                    page.artist.name,
                    page.top_tracks.len(),
                    page.albums.len(),
                    page.singles.len(),
                    t.elapsed()
                ),
                Err(e) => println!("  artist page ERROR {e}"),
            }
        }
        Err(e) => println!("Followed artists: ERROR {e}"),
    }
}
