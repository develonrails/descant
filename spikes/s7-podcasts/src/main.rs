//! Spike S7: which podcast data does the session reach, and which episodes
//! does librespot play?
//!
//!   s7-podcasts data [SEARCH TERM]
//!       Reads only; costs no audio keys. Search (does searchDesktop name
//!       shows and episodes?), a show and its episodes through spclient
//!       metadata (librespot's Show and Episode), the pathfinder operations
//!       the web player uses for shows and episodes (is there a resume
//!       point?), the library's podcasts (libraryV3) and the collection
//!       sets that might hold followed shows and Your Episodes.
//!
//! The Web API is not tried: it answers this client with 429 (spike S3).
//! Hashes are the web player's own (open.spotify.com, 4 November 2026).
//! Uses the credentials stored by spikes/s1-playback ($DESCANT_SPIKE_CACHE or
//! ~/.cache/descant-spikes/creds).

use std::{env, path::PathBuf, process::exit, time::Instant};

use descant_spotify::pathfinder::{self, Operation};
use librespot_core::{
    Error, SpotifyUri, cache::Cache, config::SessionConfig, session::Session,
};
use librespot_metadata::{Episode, Metadata, Show};
use serde_json::{Value, json};

const SHOW_METADATA: Operation = Operation {
    name: "queryShowMetadataV2",
    hash: "b475447846f37cc426add800b995a7859c169c53c72b62de6338c0c40a5dacea",
};
const PODCAST_EPISODES: Operation = Operation {
    name: "queryPodcastEpisodes",
    hash: "3539d746cf882f3909660de40b4ef472b3f5893a0761d617c282541de5d412c5",
};
const EPISODE_OR_CHAPTER: Operation = Operation {
    name: "getEpisodeOrChapter",
    hash: "5f77db47e5a2ce6680330bb61317aa09e17ddd1d189513cb218e7cd3e43288be",
};
const SEARCH_PODCASTS: Operation = Operation {
    name: "searchPodcasts",
    hash: "0195d9f61b43606d490bca64c3456e3593528cea6cc05c7e822c7c42beed0f4e",
};
/// The web player's current searchDesktop; Descant's is an older one.
const SEARCH_DESKTOP_NOW: Operation = Operation {
    name: "searchDesktop",
    hash: "eef7cc54888d91bdd6802623477873caa3948ae173a0c34fd86827b267e94c03",
};
const LIBRARY: Operation = Operation {
    name: "libraryV3",
    hash: "390c78e5b951029bad359785e69b07b536a509c581cbcd0aded5e5067f187455",
};

fn cache_dir() -> PathBuf {
    env::var_os("DESCANT_SPIKE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").expect("HOME")).join(".cache/descant-spikes/creds")
        })
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("data") => {
            let term = if args.len() > 1 { args[1..].join(" ") } else { "the daily".into() };
            data(&connect().await, &term).await
        }
        Some("episodes") => {
            let session = connect().await;
            for uri in &args[1..] {
                probe(&session, &PODCAST_EPISODES, json!({"uri": uri, "offset": 0, "limit": 3})).await;
                if let Ok(dir) = env::var("S7_DUMP_DIR") {
                    let id = uri.rsplit(':').next().unwrap_or("x");
                    let _ = std::fs::rename(format!("{dir}/queryPodcastEpisodes.json"), format!("{dir}/episodes-{id}.json"));
                }
            }
        }
        _ => {
            eprintln!("usage: s7-podcasts data [SEARCH TERM] | episodes SHOW_URI...");
            exit(2)
        }
    }
}

async fn connect() -> Session {
    let Some(credentials) = Cache::new(Some(cache_dir()), None, None, None)
        .ok()
        .and_then(|c| c.credentials())
    else {
        eprintln!("no credentials in {}; run s1-playback login first", cache_dir().display());
        exit(1)
    };
    let session = Session::new(SessionConfig::default(), None);
    session.connect(credentials, false).await.expect("connect");
    println!("connected as {}, country {}", session.username(), session.country());
    session
}

async fn data(session: &Session, term: &str) {
    // 1. Search.
    println!("\n== 1. searchDesktop '{term}'");
    let found = pathfinder::query(
        session,
        &pathfinder::SEARCH_DESKTOP,
        json!({"searchTerm": term, "offset": 0, "limit": 10, "numberOfTopResults": 5,
               "includeAudiobooks": env::var_os("S7_AUDIOBOOKS").is_some()}),
    )
    .await;
    let (mut shows, mut episodes) = (Vec::new(), Vec::new());
    match &found {
        Ok(data) => {
            if let Ok(path) = env::var("S7_DUMP") {
                std::fs::write(path, data.to_string()).expect("dump");
            }
            let root = data.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_default();
            println!("  data.{root}");
            if let Some(search) = data[&root].as_object() {
                for (key, value) in search {
                    let n = value["items"].as_array().map_or(0, Vec::len);
                    println!("  searchV2.{key}: {n} items");
                }
            }
            uris(data, "spotify:show:", &mut shows);
            uris(data, "spotify:episode:", &mut episodes);
            println!("  shows named: {}, episodes named: {}", shows.len(), episodes.len());
            for item in data[&root]["podcasts"]["items"].as_array().into_iter().flatten().take(3)
            {
                let d = &item["data"];
                println!("    show {} — {} ({})", d["name"], d["publisher"]["name"], d["uri"]);
            }
        }
        Err(e) => println!("  error: {e}"),
    }
    let vars = json!({"searchTerm": term, "offset": 0, "limit": 10, "numberOfTopResults": 5,
                      "includeAudiobooks": false, "includePreReleases": false});
    for op in [&SEARCH_PODCASTS, &SEARCH_DESKTOP_NOW] {
        match pathfinder::query(session, op, vars.clone()).await {
            Ok(data) => {
                if let Ok(dir) = env::var("S7_DUMP_DIR") {
                    std::fs::write(format!("{dir}/{}-{}.json", op.name, &op.hash[..8]), data.to_string())
                        .expect("dump");
                }
                let (mut s, mut e) = (Vec::new(), Vec::new());
                uris(&data, "spotify:show:", &mut s);
                uris(&data, "spotify:episode:", &mut e);
                let root = data.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_default();
                let sections: Vec<String> = data[&root]
                    .as_object()
                    .map(|o| o.iter().map(|(k, v)| format!("{k}:{}", v["items"].as_array().map_or(0, Vec::len))).collect())
                    .unwrap_or_default();
                println!("  {} ({}…): {} shows, {} episodes; {sections:?}", op.name, &op.hash[..8], s.len(), e.len());
            }
            Err(err) => println!("  {} ({}…): {err}", op.name, &op.hash[..8]),
        }
    }
    let Some(show_uri) = shows.first().cloned() else {
        println!("no show found; stopping");
        return;
    };

    // 2. The show and its episodes through spclient metadata.
    println!("\n== 2. spclient metadata: Show {show_uri}");
    let show_id = SpotifyUri::from_uri(&show_uri).expect("show uri");
    let t = Instant::now();
    match Show::get(session, &show_id).await {
        Ok(show) => {
            println!(
                "  '{}' by '{}' in {:?}: {} episodes, audiobook {}, media {:?}, order {:?}",
                show.name,
                show.publisher,
                t.elapsed(),
                show.episodes.len(),
                show.is_audiobook,
                show.media_type,
                show.consumption_order
            );
            for uri in show.episodes.iter().take(3) {
                episode(session, uri).await;
            }
            if let Some(first) = show.episodes.first() {
                episodes.insert(0, first.to_uri());
            }
        }
        Err(e) => println!("  error: {e}"),
    }
    for uri in episodes.iter().skip(1).take(2) {
        if let Ok(id) = SpotifyUri::from_uri(uri) {
            episode(session, &id).await;
        }
    }

    // 3. The web player's pathfinder operations for shows and episodes.
    let episode_uri = episodes.first().cloned().unwrap_or_default();
    println!("\n== 3. pathfinder");
    probe(session, &SHOW_METADATA, json!({"uri": show_uri})).await;
    probe(session, &PODCAST_EPISODES, json!({"uri": show_uri, "offset": 0, "limit": 8})).await;
    probe(session, &EPISODE_OR_CHAPTER, json!({"uri": episode_uri})).await;

    // 4. The library's podcasts.
    println!("\n== 4. library");
    probe(
        session,
        &LIBRARY,
        json!({"filters": ["Podcasts & Shows"], "order": null, "textFilter": "",
               "features": ["LIKED_SONGS", "YOUR_EPISODES"], "limit": 50, "offset": 0,
               "flatten": false, "expandedFolders": [], "folderUri": null,
               "includeFoldersWhenFlattening": true}),
    )
    .await;
    for set in ["show", "listenlater", "episode", "podcast"] {
        match collection(session, set).await {
            Ok(uris) => {
                let mut kinds = std::collections::BTreeMap::<String, usize>::new();
                for uri in &uris {
                    let kind = uri.split(':').nth(1).unwrap_or("?").to_string();
                    *kinds.entry(kind).or_default() += 1;
                }
                println!("  collection set '{set}': {} items {kinds:?}", uris.len());
            }
            Err(e) => println!("  collection set '{set}': {e}"),
        }
    }
}

/// What spclient metadata says about one episode: can librespot play it?
async fn episode(session: &Session, uri: &SpotifyUri) {
    match Episode::get(session, uri).await {
        Ok(e) => println!(
            "    episode '{}' {} min, published {:?}: {} audio files {:?}, {} videos, external '{}'",
            e.name,
            e.duration / 60_000,
            e.publish_time,
            e.audio.len(),
            e.audio.keys().collect::<Vec<_>>(),
            e.videos.len(),
            e.external_url
        ),
        Err(err) => println!("    episode {uri:?}: {err}"),
    }
}

/// Runs a pathfinder operation and prints its shape, and any field that looks
/// like a place to resume from.
async fn probe(session: &Session, op: &Operation, variables: Value) {
    let t = Instant::now();
    match pathfinder::query(session, op, variables).await {
        Ok(data) => {
            if let Ok(dir) = env::var("S7_DUMP_DIR") {
                std::fs::write(format!("{dir}/{}.json", op.name), data.to_string()).expect("dump");
            }
            let mut hits = Vec::new();
            interesting(&data, "", &mut hits);
            let top: Vec<_> = data.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
            println!("  {} in {:?}: data {top:?}, {} bytes", op.name, t.elapsed(), data.to_string().len());
            if data.to_string().len() < 600 {
                println!("    {data}");
            }
            for hit in hits.iter().take(12) {
                println!("    {hit}");
            }
        }
        Err(e) => println!("  {}: {e}", op.name),
    }
}

/// Paths of fields whose names mention playing state, resuming or progress.
fn interesting(v: &Value, path: &str, hits: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                let p = format!("{path}.{k}");
                let lower = k.to_lowercase();
                if ["played", "resume", "progress", "position", "completed", "fully"]
                    .iter()
                    .any(|w| lower.contains(w))
                {
                    let shown = child.to_string();
                    hits.push(format!("{p} = {}", &shown[..shown.len().min(140)]));
                }
                interesting(child, &p, hits);
            }
        }
        Value::Array(items) => {
            if let Some(first) = items.first() {
                interesting(first, &format!("{path}[0]"), hits);
            }
        }
        _ => {}
    }
}

/// Every string in `v` that starts with `prefix`, once each, in order.
fn uris(v: &Value, prefix: &str, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.starts_with(prefix) && !out.contains(s) => out.push(s.clone()),
        Value::Object(map) => map.values().for_each(|c| uris(c, prefix, out)),
        Value::Array(items) => items.iter().for_each(|c| uris(c, prefix, out)),
        _ => {}
    }
}

/// The uris in one collection set, first page only. The protobuf is coded by
/// hand, as in descant-spotify: PageRequest { username = 1, set = 2,
/// pagination_token = 3, limit = 4 }, PageResponse { items = 1 { uri = 1 } }.
async fn collection(session: &Session, set: &str) -> Result<Vec<String>, Error> {
    let mut request = Vec::new();
    field(&mut request, 1, session.username().as_bytes());
    field(&mut request, 2, set.as_bytes());
    request.push(4 << 3);
    varint(&mut request, 500);
    let kind = http::HeaderValue::from_static("application/vnd.collection-v2.spotify.proto");
    let mut headers = http::HeaderMap::new();
    headers.insert(http::header::CONTENT_TYPE, kind.clone());
    headers.insert(http::header::ACCEPT, kind);
    let bytes = session
        .spclient()
        .request(&http::Method::POST, "/collection/v2/paging", Some(headers), Some(&request))
        .await?;
    let mut out = Vec::new();
    for (number, item) in fields(&bytes) {
        if number == 1 {
            for (inner, value) in fields(item) {
                if inner == 1 {
                    out.push(String::from_utf8_lossy(value).into_owned());
                }
            }
        }
    }
    Ok(out)
}

fn varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 0x80 {
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn field(out: &mut Vec<u8>, number: u64, bytes: &[u8]) {
    varint(out, (number << 3) | 2);
    varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

/// The length-delimited fields of a message; others are skipped.
fn fields(mut b: &[u8]) -> Vec<(u64, &[u8])> {
    fn read(b: &mut &[u8]) -> Option<u64> {
        let (mut n, mut shift) = (0u64, 0);
        loop {
            let (&byte, rest) = b.split_first()?;
            *b = rest;
            n |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Some(n);
            }
            shift += 7;
        }
    }
    let mut out = Vec::new();
    while let Some(tag) = read(&mut b) {
        match tag & 7 {
            0 => {
                read(&mut b);
            }
            2 => {
                let Some(len) = read(&mut b) else { break };
                let len = len as usize;
                if len > b.len() {
                    break;
                }
                out.push((tag >> 3, &b[..len]));
                b = &b[len..];
            }
            1 => b = &b[8.min(b.len())..],
            5 => b = &b[4.min(b.len())..],
            _ => break,
        }
    }
    out
}
