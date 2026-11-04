//! Podcasts through the session, with the stored spike credentials; reads only.
//!
//!   cargo run -p descant-spotify --example podcasts -- the daily

use std::{env, path::PathBuf, time::Instant};

use descant_spotify::podcasts;
use librespot_core::{cache::Cache, config::SessionConfig, session::Session};

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let term = if args.is_empty() {
        "the daily".to_string()
    } else {
        args.join(" ")
    };
    let dir = PathBuf::from(env::var_os("HOME").unwrap()).join(".cache/descant-spikes/creds");
    let credentials = Cache::new(Some(dir), None, None, None)
        .unwrap()
        .credentials()
        .unwrap();
    let session = Session::new(SessionConfig::default(), None);
    session.connect(credentials, false).await.unwrap();

    let t = Instant::now();
    let shows = podcasts::search_shows(&session, &term, 5)
        .await
        .expect("search");
    println!("'{term}' in {:?}: {} shows", t.elapsed(), shows.len());
    let Some(show) = shows.first() else { return };
    println!("  show {} — {}", show.name, show.publisher);
    let page = podcasts::show(&session, &show.uri).await.expect("show");
    println!(
        "  followed: {}, {} chars of description",
        page.followed,
        page.description.len()
    );
    let episodes = podcasts::episodes(&session, &show.uri, 0, 3)
        .await
        .expect("episodes");
    println!(
        "  {} episodes in all, next page at {:?}",
        episodes.total, episodes.next
    );
    for e in &episodes.episodes {
        println!(
            "    {} ({:?}, {} ms, {:?})",
            e.name, e.released, e.duration_ms, e.progress
        );
    }
    let followed = podcasts::followed(&session).await.expect("followed");
    let saved = podcasts::saved_episodes(&session).await.expect("saved");
    println!(
        "  following {} shows, {} saved episodes",
        followed.len(),
        saved.len()
    );
}
