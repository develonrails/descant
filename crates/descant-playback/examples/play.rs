//! Drives the real controller against a real account, without a UI.
//!
//!   cargo run -p descant-playback --example play [CONTEXT_URI]
//!
//! Uses the credentials stored by spikes/s1-playback ($DESCANT_SPIKE_CACHE or
//! ~/.cache/descant-spikes/creds). Loads the context, then: next track, pause,
//! resume, and a burst of three skips (sent as one immediate skip plus one
//! skip of two). Prints every status, notice and track.

use std::{env, path::PathBuf, time::Duration};

use descant_core::supervisor::Command;
use descant_playback::{
    controller::{self, ControllerConfig, Update},
    tap,
};
use librespot_core::cache::Cache;
use tokio::{sync::mpsc, time::Instant};

#[tokio::main]
async fn main() {
    tap::install();
    let context = env::args()
        .nth(1)
        .unwrap_or_else(|| "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M".to_string());
    let dir = env::var_os("DESCANT_SPIKE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap()).join(".cache/descant-spikes/creds")
        });
    let credentials = Cache::new(Some(dir), None, None, None)
        .ok()
        .and_then(|c| c.credentials())
        .expect("no stored credentials; run spikes/s1-playback login first");

    let (updates_tx, mut updates) = mpsc::unbounded_channel();
    let config = ControllerConfig {
        device_name: "Descant (example)".into(),
        ..Default::default()
    };
    let handle = controller::spawn(credentials, config, updates_tx);

    let start = Instant::now();
    tokio::spawn(async move {
        while let Some(update) = updates.recv().await {
            let t = start.elapsed().as_secs_f64();
            match update {
                Update::NowPlaying(info) => {
                    println!("{t:>6.1}s  ♪ {} — {}", info.title, info.artist_line())
                }
                Update::Status(status) => println!("{t:>6.1}s  status {status:?}"),
                Update::Notice(notice) => println!("{t:>6.1}s  notice {notice:?}"),
                Update::Error(e) => println!("{t:>6.1}s  error {e}"),
                Update::Session(_) => println!("{t:>6.1}s  session ready"),
                Update::Queue(q) => println!("{t:>6.1}s  queue: {} upcoming", q.len()),
                Update::Volume(v) => println!("{t:>6.1}s  volume {v}"),
                Update::Resume { point, playing } => println!(
                    "{t:>6.1}s  at {} {} ms{}",
                    point.track,
                    point.position_ms,
                    if playing { ", playing" } else { "" }
                ),
            }
        }
    });

    let script: &[(u64, &str, Command)] = &[
        (
            2,
            "load",
            Command::Load {
                context,
                start_track: None,
            },
        ),
        (12, "next", Command::Skip(1)),
        (20, "pause", Command::Pause),
        (23, "play", Command::Play),
        (30, "skip burst 1/3", Command::Skip(1)),
        (30, "skip burst 2/3", Command::Skip(1)),
        (30, "skip burst 3/3", Command::Skip(1)),
    ];
    for (at, label, command) in script.iter().cloned() {
        tokio::time::sleep_until(start + Duration::from_secs(at)).await;
        println!("{:>6.1}s  > {label}", start.elapsed().as_secs_f64());
        handle.send(command);
    }
    tokio::time::sleep_until(start + Duration::from_secs(42)).await;
    handle.send(Command::Pause);
    tokio::time::sleep(Duration::from_millis(500)).await;
    println!("done");
}
