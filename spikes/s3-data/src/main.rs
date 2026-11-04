//! Spike S3/S4 — which data sources answer with a token derived from the
//! librespot session, and what do they return?
//!
//! Read-only GETs. Tokens never leave the process; only status codes and
//! shape summaries are printed. Reuses the credentials stored by
//! `s1-playback login` ($DESCANT_SPIKE_CACHE or ~/.cache/descant-spikes/creds).

use std::{env, path::PathBuf, process::exit, time::Duration};

use librespot_core::{
    cache::Cache, config::SessionConfig, session::Session, spotify_id::SpotifyId,
    spotify_uri::SpotifyUri,
};
use librespot_oauth::DeviceAuthClientBuilder;
use serde_json::Value;

const SCOPES: &str = "user-read-private,user-library-read,playlist-read-private,\
playlist-read-collaborative,user-top-read,user-read-recently-played,user-follow-read";

/// Spotify-owned editorial playlist (Today's Top Hits) and a stable artist (Radiohead).
const EDITORIAL: &str = "37i9dQZF1DXcBWIGoYBM5M";
const ARTIST: &str = "4Z8W4fKeB5YxbusRsdQVPb";
const TRACK_A: &str = "6rqhFgbbKwnb9MLmUQDhG6"; // Speechless — any two tracks will do
const TRACK_B: &str = "3n3Ppam7vgaVa1iaRUc9Lp"; // Mr. Brightside

fn cache_dir() -> PathBuf {
    env::var_os("DESCANT_SPIKE_CACHE").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env::var_os("HOME").expect("HOME")).join(".cache/descant-spikes/creds")
    })
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    exit(1)
}

#[tokio::main]
async fn main() {
    let cache = Cache::new(Some(cache_dir()), None, None, None).unwrap_or_else(|e| fail(&e.to_string()));
    let creds = cache.credentials().unwrap_or_else(|| fail("no stored credentials — run s1-playback login"));
    let session = Session::new(SessionConfig::default(), Some(cache));
    session.connect(creds, true).await.unwrap_or_else(|e| fail(&e.to_string()));
    println!("session: user {} country {}", session.username(), session.country());

    // Two ways to get a bearer token out of a logged-in session.
    let keymaster = session.token_provider().get_token(SCOPES).await;
    let login5 = session.login5().auth_token().await;
    let mut tokens: Vec<(&str, String)> = Vec::new();
    match &keymaster {
        Ok(t) => {
            println!("keymaster token: ok, expires in {:?}, scopes {}", t.expires_in, t.scopes.len());
            tokens.push(("keymaster", t.access_token.clone()));
        }
        Err(e) => println!("keymaster token: FAILED {e}"),
    }
    match &login5 {
        Ok(t) => {
            println!("login5 token: ok, expires in {:?}", t.expires_in);
            if env::var_os("S3_SKIP_LOGIN5").is_none() {
                tokens.push(("login5", t.access_token.clone()));
            }
        }
        Err(e) => println!("login5 token: FAILED {e}"),
    }
    // S3_OAUTH=1: also get a plain OAuth token for the desktop client id via
    // the device flow (what Riff uses). Kept in memory only.
    if env::var_os("S3_OAUTH").is_some() {
        let scopes: Vec<&str> = SCOPES.split(',').chain(["streaming"]).collect();
        let client = DeviceAuthClientBuilder::new(&SessionConfig::default().client_id, scopes)
            .build()
            .unwrap_or_else(|e| fail(&e.to_string()));
        let auth = client.request_device_code_async().await.unwrap_or_else(|e| fail(&e.to_string()));
        println!("LOGIN_URL {}", auth.url());
        println!("LOGIN_CODE {}", auth.user_code());
        let t = client.poll_for_token_async(&auth).await.unwrap_or_else(|e| fail(&e.to_string()));
        println!("oauth token: ok, scopes {}", t.scopes.len());
        tokens.push(("oauth", t.access_token));
    }

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();

    let me = session.username();
    let probes: Vec<(&str, String)> = vec![
        ("me", "/me".into()),
        ("search limit=50", "/search?q=radiohead&type=track,album,artist,playlist&limit=50".into()),
        ("search limit=10", "/search?q=radiohead&type=track,album,artist,playlist&limit=10".into()),
        ("search 'Daily Mix'", "/search?q=Daily%20Mix&type=playlist&limit=10".into()),
        ("search 'Discover Weekly'", "/search?q=Discover%20Weekly&type=playlist&limit=10".into()),
        ("editorial playlist", format!("/playlists/{EDITORIAL}")),
        ("editorial items", format!("/playlists/{EDITORIAL}/items?limit=5")),
        ("editorial tracks (old)", format!("/playlists/{EDITORIAL}/tracks?limit=5")),
        ("my playlists", "/me/playlists?limit=50".into()),
        ("other user's playlists", "/users/spotify/playlists?limit=5".into()),
        ("liked tracks", "/me/tracks?limit=5".into()),
        ("saved albums", "/me/albums?limit=5".into()),
        ("followed artists", "/me/following?type=artist&limit=5".into()),
        ("top artists", "/me/top/artists?limit=5".into()),
        ("top tracks", "/me/top/tracks?limit=5".into()),
        ("recently played", "/me/player/recently-played?limit=5".into()),
        ("artist", format!("/artists/{ARTIST}")),
        ("artist albums", format!("/artists/{ARTIST}/albums?limit=5")),
        ("artist top tracks", format!("/artists/{ARTIST}/top-tracks?market=NL")),
        ("related artists", format!("/artists/{ARTIST}/related-artists")),
        ("batch tracks", format!("/tracks?ids={TRACK_A},{TRACK_B}")),
        ("new releases", "/browse/new-releases?limit=5".into()),
        ("featured playlists", "/browse/featured-playlists?limit=5".into()),
        ("categories", "/browse/categories?limit=5".into()),
        ("recommendations", format!("/recommendations?seed_artists={ARTIST}&limit=5")),
    ];
    let _ = me;
    // S3_ONLY=substring limits the probes; S3_DELAY_MS spaces them out.
    let only = env::var("S3_ONLY").ok();
    let delay = env::var("S3_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(250u64);
    let probes: Vec<(&str, String)> = probes
        .into_iter()
        .filter(|(n, _)| only.as_deref().is_none_or(|o| o.split(',').any(|x| n.contains(x))))
        .collect();

    for (label, token) in &tokens {
        println!();
        println!("== Web API with {label} token");
        for (name, path) in &probes {
            let url = format!("https://api.spotify.com/v1{path}");
            let resp = http.get(&url).bearer_auth(token).send().await;
            match resp {
                Err(e) => println!("  {name:<26} ERR  {e}"),
                Ok(r) => {
                    let status = r.status().as_u16();
                    let retry = r
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .map(|v| format!(" retry-after={v}"))
                        .unwrap_or_default();
                    let body: Value = r.json().await.unwrap_or(Value::Null);
                    println!("  {name:<26} {status}{retry}  {}", summarise(&body));
                }
            }
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }

    // S3_PATHFINDER=1: Spotify's internal GraphQL (what the official client
    // uses for Home), with the session's login5 token + client token.
    // Persisted-query hashes as used by Spotifast (crmne/spotifast, MIT).
    if env::var_os("S3_PATHFINDER").is_some() {
        println!();
        println!("== pathfinder v2 (api-partner) with login5 token");
        let bearer = login5.as_ref().map(|t| t.access_token.clone()).unwrap_or_default();
        let client_token = session.spclient().client_token().await.unwrap_or_default();
        let country = session.country();
        let queries = vec![
            ("new releases (queryWhatsNewFeed)", serde_json::json!({
                "operationName": "queryWhatsNewFeed",
                "variables": {"offset": 0, "limit": 20, "onlyUnPlayedItems": false,
                              "includedContentTypes": ["ALBUM"], "includeEpisodeContentRatingsV2": false},
                "extensions": {"persistedQuery": {"version": 1,
                    "sha256Hash": "d889c8c936ab192af8ced595427f5ba2acdf63478fdc0a181c8d477f8322630e"}}
            })),
            ("Made for You (homeSection)", serde_json::json!({
                "operationName": "homeSection",
                "variables": {"uri": "spotify:section:0JQ5DAUnp4wcj0bCb3wh3S", "country": country,
                              "timeZone": "Europe/Amsterdam", "sp_t": "",
                              "sectionItemsLimit": 20, "sectionItemsOffset": 0},
                "extensions": {"persistedQuery": {"version": 1,
                    "sha256Hash": "eb3fba2d388cf4fc4d696b1757a58584e9538a3b515ea742e9cc9465807340be"}}
            })),
            ("Top mixes (homeSection)", serde_json::json!({
                "operationName": "homeSection",
                "variables": {"uri": "spotify:section:0JQ5DAnM3wGh0gz1MXnu89", "country": country,
                              "timeZone": "Europe/Amsterdam", "sp_t": "",
                              "sectionItemsLimit": 20, "sectionItemsOffset": 0},
                "extensions": {"persistedQuery": {"version": 1,
                    "sha256Hash": "eb3fba2d388cf4fc4d696b1757a58584e9538a3b515ea742e9cc9465807340be"}}
            })),
            // Hash as measured by ralph/Spotifly (MIT), 2026-10-21.
            ("top (userTopContent)", serde_json::json!({
                "operationName": "userTopContent",
                "variables": {
                    "includeTopArtists": true,
                    "topArtistsInput": {"offset": 0, "limit": 10, "sortBy": "AFFINITY", "timeRange": "SHORT_TERM"},
                    "includeTopTracks": true,
                    "topTracksInput": {"offset": 0, "limit": 10, "sortBy": "AFFINITY", "timeRange": "SHORT_TERM"}
                },
                "extensions": {"persistedQuery": {"version": 1,
                    "sha256Hash": "61b7353d3625780022dbc63d459eb64cd609e7e5e2a820f9160d65030efe2e15"}}
            })),
            // Hash as used by sjdonado/idonthavespotify (MIT).
            ("search (searchDesktop)", serde_json::json!({
                "operationName": "searchDesktop",
                "variables": {"searchTerm": "radiohead", "offset": 0, "limit": 20,
                              "numberOfTopResults": 5, "includeAudiobooks": false},
                "extensions": {"persistedQuery": {"version": 1,
                    "sha256Hash": "75bbf6bfcfdf85b8fc828417bfad92b7cd66bf7f556d85670f4da8292373ebec"}}
            })),
        ];
        for (name, q) in queries {
            let r = http
                .post("https://api-partner.spotify.com/pathfinder/v2/query")
                .bearer_auth(&bearer)
                .header("client-token", &client_token)
                .json(&q)
                .send()
                .await;
            match r {
                Err(e) => println!("  {name:<34} ERR {e}"),
                Ok(r) => {
                    let status = r.status().as_u16();
                    let body: Value = r.json().await.unwrap_or(Value::Null);
                    if let Some(dir) = env::var_os("S3_DUMP") {
                        let file = PathBuf::from(dir).join(format!("{}.json", name.split(' ').next().unwrap_or("q")));
                        let _ = std::fs::write(file, serde_json::to_vec_pretty(&body).unwrap_or_default());
                    }
                    let mut counts = std::collections::BTreeMap::new();
                    let mut names = Vec::new();
                    walk(&body, &mut counts, &mut names);
                    let errs = body.get("errors").map(|e| e.to_string().chars().take(140).collect::<String>());
                    println!("  {name:<34} {status}  uris {counts:?}  e.g. {:?}{}", &names[..names.len().min(4)],
                        errs.map(|e| format!("  errors {e}")).unwrap_or_default());
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    println!();
    println!("== spclient through the session (no Web API involved)");
    let sp = session.spclient();
    let recent = format!(
        "/recently-played/v3/user/{}/recently-played?format=json&offset=0&limit=20&filter=default,collection-new-episodes",
        session.username()
    );
    match sp.request(&http::Method::GET, &recent, None, None).await {
        Ok(b) => {
            let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
            if let Some(dir) = env::var_os("S3_DUMP") {
                let _ = std::fs::write(PathBuf::from(dir).join("recent.json"), serde_json::to_vec_pretty(&v).unwrap_or_default());
            }
            let mut counts = std::collections::BTreeMap::new();
            let mut names = Vec::new();
            walk(&v, &mut counts, &mut names);
            println!("  recently-played v3          ok, uris {counts:?}");
        }
        Err(e) => println!("  recently-played v3          FAILED {e}"),
    }
    match sp.get_rootlist(0, Some(200)).await {
        Ok(b) => println!("  rootlist (my playlists)     ok, {} bytes protobuf", b.len()),
        Err(e) => println!("  rootlist (my playlists)     FAILED {e}"),
    }
    let editorial = SpotifyId::from_base62(EDITORIAL).unwrap();
    match sp.get_playlist(&editorial).await {
        Ok(b) => println!("  editorial playlist          ok, {} bytes protobuf", b.len()),
        Err(e) => println!("  editorial playlist          FAILED {e}"),
    }
    let track = SpotifyUri::Track { id: SpotifyId::from_base62(TRACK_A).unwrap() };
    match sp.get_radio_for_track(&track).await {
        Ok(b) => {
            let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
            println!("  radio for track             ok, {}", summarise(&v));
        }
        Err(e) => println!("  radio for track             FAILED {e}"),
    }
    let artist = SpotifyUri::Artist { id: SpotifyId::from_base62(ARTIST).unwrap() };
    match sp.get_artist_metadata(&artist).await {
        Ok(b) => println!("  artist metadata             ok, {} bytes protobuf", b.len()),
        Err(e) => println!("  artist metadata             FAILED {e}"),
    }
}

/// Counts spotify: URIs by kind and collects a few names, anywhere in the tree.
fn walk(v: &Value, counts: &mut std::collections::BTreeMap<String, usize>, names: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            if let Some(uri) = o.get("uri").and_then(Value::as_str) {
                if let Some(kind) = uri.strip_prefix("spotify:").and_then(|r| r.split(':').next()) {
                    *counts.entry(kind.to_string()).or_default() += 1;
                    if let Some(n) = o.get("name").and_then(Value::as_str) {
                        if names.len() < 8 && !names.iter().any(|x| x == n) {
                            names.push(n.chars().take(28).collect());
                        }
                    }
                }
            }
            for val in o.values() {
                walk(val, counts, names);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| walk(x, counts, names)),
        _ => {}
    }
}

/// A one-line shape summary: error message, or the item counts that matter.
fn summarise(v: &Value) -> String {
    if let Some(err) = v.get("error") {
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
        return format!("error: {msg}");
    }
    let mut parts = Vec::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            if let Some(items) = val.get("items").and_then(Value::as_array) {
                let total = val.get("total").and_then(Value::as_u64);
                parts.push(format!("{k}: {} items{}", items.len(), total.map(|t| format!(" of {t}")).unwrap_or_default()));
            } else if let Some(arr) = val.as_array() {
                parts.push(format!("{k}: {} entries", arr.len()));
            }
        }
        if let Some(items) = obj.get("items").and_then(Value::as_array) {
            let owners: Vec<String> = items
                .iter()
                .filter_map(|i| i.get("owner").and_then(|o| o.get("id")).and_then(Value::as_str).map(String::from))
                .take(3)
                .collect();
            let names: Vec<String> = items
                .iter()
                .filter_map(|i| i.get("name").and_then(Value::as_str).map(|s| s.chars().take(24).collect()))
                .take(3)
                .collect();
            parts.push(format!("items: {}", items.len()));
            if !names.is_empty() {
                parts.push(format!("e.g. {names:?}"));
            }
            if !owners.is_empty() {
                parts.push(format!("owners {owners:?}"));
            }
        }
        if obj.contains_key("name") && parts.is_empty() {
            parts.push(format!("name={:?}", obj["name"].as_str().unwrap_or("")));
        }
        if parts.is_empty() {
            let keys: Vec<&String> = obj.keys().take(6).collect();
            parts.push(format!("keys {keys:?}"));
        }
    }
    parts.join(", ")
}
