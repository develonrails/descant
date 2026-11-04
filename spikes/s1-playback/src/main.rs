//! Spike S1 — does this account play through librespot, and how does it fail?
//!
//!   s1-playback login
//!       Device-code login (enter the code at spotify.com/pair). Stores
//!       reusable credentials in the cache dir; nothing else needs a login.
//!
//!   s1-playback play  [PLAYLIST_URI] [--tracks N] [--seconds S] [--pulse]
//!       Plays the first S seconds of N tracks and checks that real audio came
//!       out of the decoder (sample count and RMS), not just a "Playing" event.
//!
//!   s1-playback probe [PLAYLIST_URI] --interval I [--tracks N] [--seconds S]
//!       Starts one track every I seconds, to measure how fast the audio-key
//!       limit recovers.
//!
//!   s1-playback connect [PLAYLIST_URI] [--tracks N] [--seconds S]
//!       S1b: drives playback through Spirc (librespot-connect) instead of the
//!       Player directly: load a context, next, pause/resume, add to queue.
//!       Takes over the account's active device while it runs.
//!
//!   s1-playback storm [PLAYLIST_URI] [--tracks N] [--seconds S] [--pulse]
//!       Same, but skips every S seconds (default 1.5) to provoke the
//!       audio-key storm reported in librespot#1649.
//!
//! By default audio goes to a measuring sink paced at real time, so nothing is
//! heard. --pulse sends it to PulseAudio/PipeWire instead (no measurement).
//! The cache dir is $DESCANT_SPIKE_CACHE or ~/.cache/descant-spikes/creds.

use std::{
    env,
    path::PathBuf,
    process::exit,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use librespot_core::{
    authentication::Credentials, cache::Cache, config::SessionConfig, session::Session,
    spotify_uri::SpotifyUri,
};
use librespot_metadata::{Metadata, Playlist, Track};
use librespot_oauth::DeviceAuthClientBuilder;
use librespot_playback::{
    audio_backend::{self, Sink, SinkResult},
    config::{AudioFormat, PlayerConfig},
    convert::Converter,
    decoder::AudioPacket,
    mixer::NoOpVolume,
    player::{Player, PlayerEvent},
};
use librespot_connect::{ConnectConfig, LoadRequest, LoadRequestOptions, Spirc};
use librespot_playback::mixer::{self, MixerConfig};
use log::{Level, LevelFilter, Log, Record};

/// Today's Top Hits: public, always populated, no library needed.
const DEFAULT_PLAYLIST: &str = "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M";

const SCOPES: &[&str] = &[
    "streaming",
    "user-read-private",
    "user-read-email",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    "user-library-read",
    "user-library-modify",
    "user-top-read",
    "user-read-recently-played",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-follow-read",
];

const SAMPLE_RATE: f64 = 44_100.0;
const CHANNELS: f64 = 2.0;

// ---------------------------------------------------------------- logging --

/// Prints warnings and errors, and counts the two lines that matter here.
struct CountingLogger {
    start: Instant,
    verbose: bool,
    key_errors: AtomicU64,
    decrypt_skipped: AtomicU64,
}

impl Log for CountingLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= Level::Debug
    }

    fn log(&self, r: &Record) {
        let target = r.target();
        if target.starts_with("symphonia") {
            return; // junk-skipping spam from garbage data; the counters tell the story
        }
        let msg = r.args().to_string();
        if target.starts_with("librespot_core::audio_key") && r.level() == Level::Error {
            self.key_errors.fetch_add(1, Ordering::Relaxed);
        }
        if msg.contains("Unable to load key") {
            self.decrypt_skipped.fetch_add(1, Ordering::Relaxed);
        }
        let show = r.level() <= Level::Warn
            || (r.level() <= Level::Info && (self.verbose || target.starts_with("s1_playback")))
            || (self.verbose && r.level() <= Level::Debug && target.starts_with("librespot"));
        if show {
            eprintln!(
                "  [{:>7.2}s {:<5} {}] {}",
                self.start.elapsed().as_secs_f64(),
                r.level(),
                target,
                msg
            );
        }
    }

    fn flush(&self) {}
}

fn init_logger(verbose: bool) -> &'static CountingLogger {
    let logger: &'static CountingLogger = Box::leak(Box::new(CountingLogger {
        start: Instant::now(),
        verbose,
        key_errors: AtomicU64::new(0),
        decrypt_skipped: AtomicU64::new(0),
    }));
    log::set_logger(logger).expect("logger already set");
    log::set_max_level(LevelFilter::Debug);
    logger
}

// ----------------------------------------------------------- measuring sink --

#[derive(Default)]
struct Meter {
    samples: u64,
    sum_sq: f64,
    peak: f64,
}

impl Meter {
    fn audio_seconds(&self) -> f64 {
        self.samples as f64 / CHANNELS / SAMPLE_RATE
    }

    fn rms_dbfs(&self) -> f64 {
        if self.samples == 0 {
            return f64::NEG_INFINITY;
        }
        10.0 * (self.sum_sq / self.samples as f64).log10()
    }
}

/// Accepts decoded audio at real-time pace and measures it.
struct MeterSink {
    meter: Arc<Mutex<Meter>>,
    started: Option<Instant>,
    written_secs: f64,
}

impl Sink for MeterSink {
    fn start(&mut self) -> SinkResult<()> {
        self.started = Some(Instant::now());
        self.written_secs = 0.0;
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.started = None;
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        if let AudioPacket::Samples(samples) = packet {
            {
                let mut m = self.meter.lock().unwrap();
                for s in samples.iter() {
                    m.sum_sq += s * s;
                    m.peak = m.peak.max(s.abs());
                }
                m.samples += samples.len() as u64;
            }
            // Pace like a sound card would, so downloads and key requests
            // happen at the rate a real player causes them.
            self.written_secs += samples.len() as f64 / CHANNELS / SAMPLE_RATE;
            if let Some(start) = self.started {
                let ahead = self.written_secs - start.elapsed().as_secs_f64();
                if ahead > 0.0 {
                    std::thread::sleep(Duration::from_secs_f64(ahead));
                }
            }
        }
        Ok(())
    }
}

// --------------------------------------------------------------------- cli --

struct Args {
    command: String,
    playlist: String,
    tracks: usize,
    seconds: f64,
    pulse: bool,
    verbose: bool,
    /// probe mode: start a track every N seconds (and stay idle in between)
    interval: Option<f64>,
}

fn parse_args() -> Args {
    let mut it = env::args().skip(1);
    let command = it.next().unwrap_or_else(|| usage());
    let storm = command == "storm";
    let mut args = Args {
        command,
        playlist: DEFAULT_PLAYLIST.to_string(),
        tracks: if storm { 30 } else { 10 },
        seconds: if storm { 1.5 } else { 8.0 },
        pulse: false,
        verbose: false,
        interval: None,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tracks" => args.tracks = it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            "--seconds" => args.seconds = it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            "--pulse" => args.pulse = true,
            "--interval" => args.interval = Some(it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage())),
            "-v" | "--verbose" => args.verbose = true,
            s if s.starts_with("spotify:") => args.playlist = s.to_string(),
            _ => usage(),
        }
    }
    args
}

fn usage() -> ! {
    eprintln!("usage: s1-playback login | play|storm|probe|connect [PLAYLIST_URI] [--tracks N] [--seconds S] [--interval S] [--pulse] [-v]");
    exit(2)
}

fn cache_dir() -> PathBuf {
    env::var_os("DESCANT_SPIKE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").expect("HOME")).join(".cache/descant-spikes/creds"))
}

// -------------------------------------------------------------------- main --

#[tokio::main]
async fn main() {
    let args = parse_args();
    let logger = init_logger(args.verbose);
    match args.command.as_str() {
        "login" => login().await,
        "play" | "storm" | "probe" => run(&args, logger).await,
        "connect" => connect(&args, logger).await,
        _ => usage(),
    }
}

async fn login() {
    let config = SessionConfig::default();
    let client = DeviceAuthClientBuilder::new(&config.client_id, SCOPES.to_vec())
        .build()
        .unwrap_or_else(|e| fail(&format!("device auth client: {e}")));
    let auth = client
        .request_device_code_async()
        .await
        .unwrap_or_else(|e| fail(&format!("device code: {e}")));
    println!("LOGIN_URL {}", auth.url());
    println!("LOGIN_CODE {}", auth.user_code());
    let token = client
        .poll_for_token_async(&auth)
        .await
        .unwrap_or_else(|e| fail(&format!("token: {e}")));

    let cache = Cache::new(Some(cache_dir()), None, None, None)
        .unwrap_or_else(|e| fail(&format!("cache: {e}")));
    let session = Session::new(config, Some(cache));
    session
        .connect(Credentials::with_access_token(token.access_token), true)
        .await
        .unwrap_or_else(|e| fail(&format!("connect: {e}")));
    println!("Logged in as '{}', country {}. Credentials stored in {}", session.username(), session.country(), cache_dir().display());
}

#[derive(PartialEq)]
enum Outcome {
    Ok,
    Silent,
    NoAudio,
    Unavailable,
}

async fn run(args: &Args, logger: &'static CountingLogger) {
    let cache = Cache::new(Some(cache_dir()), None, None, None)
        .unwrap_or_else(|e| fail(&format!("cache: {e}")));
    let credentials = cache
        .credentials()
        .unwrap_or_else(|| fail("no stored credentials — run `s1-playback login` first"));
    let session = Session::new(SessionConfig::default(), Some(cache));
    session
        .connect(credentials, true)
        .await
        .unwrap_or_else(|e| fail(&format!("connect: {e}")));
    log::info!("connected as '{}' ({})", session.username(), session.country());

    let uri = SpotifyUri::from_uri(&args.playlist).unwrap_or_else(|_| fail("bad playlist URI"));
    let playlist = Playlist::get(&session, &uri)
        .await
        .unwrap_or_else(|e| fail(&format!("playlist: {e}")));
    let tracks: Vec<SpotifyUri> = playlist
        .tracks()
        .filter(|t| matches!(t, SpotifyUri::Track { .. }))
        .take(args.tracks)
        .cloned()
        .collect();
    log::info!("{} tracks from {}", tracks.len(), args.playlist);

    let meter = Arc::new(Mutex::new(Meter::default()));
    let player = if args.pulse {
        let backend = audio_backend::find(Some("pulseaudio".to_string()))
            .unwrap_or_else(|| fail("pulseaudio backend not compiled in"));
        Player::new(PlayerConfig::default(), session.clone(), Box::new(NoOpVolume), move || {
            backend(None, AudioFormat::default())
        })
    } else {
        let meter = meter.clone();
        Player::new(PlayerConfig::default(), session.clone(), Box::new(NoOpVolume), move || {
            Box::new(MeterSink { meter, started: None, written_secs: 0.0 }) as Box<dyn Sink>
        })
    };
    let mut events = player.get_player_event_channel();

    let window = Duration::from_secs_f64(args.seconds);
    let mut results = Vec::new();
    println!();
    println!("  t(s)   #  result       start   audio    RMS dBFS  key-err  track");

    for (i, track) in tracks.iter().enumerate() {
        *meter.lock().unwrap() = Meter::default();
        let keys_before = logger.key_errors.load(Ordering::Relaxed);
        let t0 = Instant::now();
        let mut started_after = None;
        let mut unavailable = false;

        player.load(track.clone(), true, 0);
        while t0.elapsed() < window {
            let remaining = window.saturating_sub(t0.elapsed());
            match tokio::time::timeout(remaining, events.recv()).await {
                Ok(Some(PlayerEvent::Playing { track_id, .. })) if &track_id == track => {
                    started_after.get_or_insert(t0.elapsed());
                }
                Ok(Some(PlayerEvent::Unavailable { track_id, .. })) if &track_id == track => {
                    unavailable = true;
                    break;
                }
                Ok(Some(PlayerEvent::EndOfTrack { track_id, .. })) if &track_id == track => break,
                Ok(Some(_)) => {}
                Ok(None) => fail("player event channel closed"),
                Err(_) => break, // window elapsed
            }
        }

        let m = meter.lock().unwrap();
        let key_errors = logger.key_errors.load(Ordering::Relaxed) - keys_before;
        let outcome = if unavailable {
            Outcome::Unavailable
        } else if args.pulse {
            if started_after.is_some() { Outcome::Ok } else { Outcome::NoAudio }
        } else if m.audio_seconds() < 0.5 {
            Outcome::NoAudio
        } else if m.rms_dbfs() < -60.0 {
            Outcome::Silent
        } else {
            Outcome::Ok
        };
        let name = Track::get(&session, track)
            .await
            .map(|t| t.name)
            .unwrap_or_else(|_| track.to_uri());
        println!(
            "{:>6.1}  {:>2}  {:<11} {:>6}  {:>5.1}s  {:>9}  {:>7}  {}",
            logger.start.elapsed().as_secs_f64(),
            i + 1,
            match outcome {
                Outcome::Ok => "OK",
                Outcome::Silent => "SILENT",
                Outcome::NoAudio => "NO AUDIO",
                Outcome::Unavailable => "UNAVAILABLE",
            },
            started_after.map(|d| format!("{:.2}s", d.as_secs_f64())).unwrap_or_else(|| "-".into()),
            m.audio_seconds(),
            if args.pulse { "n/a".to_string() } else { format!("{:.1}", m.rms_dbfs()) },
            key_errors,
            name
        );
        results.push((outcome, key_errors));
        drop(m);

        if let Some(interval) = args.interval {
            player.stop();
            let target = Duration::from_secs_f64(interval);
            if t0.elapsed() < target {
                tokio::time::sleep(target - t0.elapsed()).await;
            }
        }
    }
    player.stop();

    let ok = results.iter().filter(|(o, _)| *o == Outcome::Ok).count();
    let total_keys = logger.key_errors.load(Ordering::Relaxed);
    let skipped = logger.decrypt_skipped.load(Ordering::Relaxed);
    println!();
    println!("OK {ok}/{}  audio-key errors {total_keys}  played-without-decryption {skipped}", results.len());
    let first = results.iter().take(3);
    let verdict = if ok == 0 && results.len() >= 3 && first.clone().all(|(_, k)| *k > 0) {
        "VERDICT playplay-likely (every track fails on its audio key)"
    } else if ok == results.len() {
        "VERDICT plays"
    } else if total_keys > 0 {
        "VERDICT plays-with-key-errors (intermittent; see librespot#1649)"
    } else {
        "VERDICT plays-with-other-failures"
    };
    println!("{verdict}");
}

/// Audio measured over `window`, while watching for the track that plays.
async fn measure(
    meter: &Arc<Mutex<Meter>>,
    events: &mut librespot_playback::player::PlayerEventChannel,
    window: Duration,
) -> (f64, f64, Option<SpotifyUri>, bool) {
    *meter.lock().unwrap() = Meter::default();
    let t0 = Instant::now();
    let mut playing = None;
    let mut unavailable = false;
    while t0.elapsed() < window {
        let remaining = window.saturating_sub(t0.elapsed());
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(PlayerEvent::Playing { track_id, .. })) => playing = Some(track_id),
            Ok(Some(PlayerEvent::Unavailable { .. })) => unavailable = true,
            Ok(Some(_)) => {}
            Ok(None) => fail("player event channel closed"),
            Err(_) => break,
        }
    }
    let m = meter.lock().unwrap();
    (m.audio_seconds(), m.rms_dbfs(), playing, unavailable)
}

async fn connect(args: &Args, logger: &'static CountingLogger) {
    let cache = Cache::new(Some(cache_dir()), None, None, None)
        .unwrap_or_else(|e| fail(&format!("cache: {e}")));
    let credentials = cache
        .credentials()
        .unwrap_or_else(|| fail("no stored credentials — run `s1-playback login` first"));
    let session = Session::new(SessionConfig::default(), Some(cache));
    let mixer = mixer::find(None).unwrap()(MixerConfig::default())
        .unwrap_or_else(|e| fail(&format!("mixer: {e}")));

    let meter = Arc::new(Mutex::new(Meter::default()));
    let sink_meter = meter.clone();
    let player = Player::new(PlayerConfig::default(), session.clone(), mixer.get_soft_volume(), move || {
        Box::new(MeterSink { meter: sink_meter, started: None, written_secs: 0.0 }) as Box<dyn Sink>
    });
    let mut events = player.get_player_event_channel();

    let config = ConnectConfig { name: "Descant S1b".to_string(), ..Default::default() };
    let (spirc, task) = Spirc::new(config, session.clone(), credentials, player, mixer)
        .await
        .unwrap_or_else(|e| fail(&format!("spirc: {e}")));
    let task = tokio::spawn(task);
    log::info!("spirc up as '{}' ({})", session.username(), session.country());

    let window = Duration::from_secs_f64(args.seconds);
    let mut report = Vec::new();
    let mut check = |step: &str, audio: f64, rms: f64, track: Option<SpotifyUri>, unavailable: bool, expect_audio: bool| {
        let ok = if expect_audio { audio > 0.5 * window.as_secs_f64() && rms > -60.0 && !unavailable } else { audio < 0.2 };
        println!(
            "{:<28} {:<4} audio {:>4.1}s  rms {:>6.1}  track {}",
            step,
            if ok { "OK" } else { "FAIL" },
            audio,
            rms,
            track.map(|t| t.to_uri()).unwrap_or_else(|| "-".into())
        );
        report.push(ok);
    };

    spirc.activate().unwrap();
    spirc
        .load(LoadRequest::from_context_uri(
            args.playlist.clone(),
            LoadRequestOptions { start_playing: true, ..Default::default() },
        ))
        .unwrap();
    let (a, r, t, u) = measure(&meter, &mut events, window).await;
    check("load context + play", a, r, t, u, true);

    for i in 1..args.tracks.max(2) {
        spirc.next().unwrap();
        let (a, r, t, u) = measure(&meter, &mut events, window).await;
        check(&format!("next #{i}"), a, r, t, u, true);
    }

    spirc.pause().unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (a, r, t, u) = measure(&meter, &mut events, Duration::from_secs(2)).await;
    check("pause (expect silence)", a, r, t, u, false);

    spirc.play().unwrap();
    let (a, r, t, u) = measure(&meter, &mut events, window).await;
    check("resume", a, r, t, u, true);

    // Queue a specific track and skip to it.
    let queued = SpotifyUri::from_uri("spotify:track:3n3Ppam7vgaVa1iaRUc9Lp").unwrap();
    spirc.add_to_queue(queued.clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    spirc.next().unwrap();
    let (a, r, t, u) = measure(&meter, &mut events, window).await;
    let hit = t.as_ref() == Some(&queued);
    check(&format!("queued track plays ({})", if hit { "right track" } else { "WRONG track" }), a, r, t, u, hit);

    spirc.pause().unwrap();
    let _ = spirc.disconnect(true);
    let _ = spirc.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;

    let ok = report.iter().filter(|x| **x).count();
    println!();
    println!(
        "SPIRC {ok}/{} steps OK  audio-key errors {}",
        report.len(),
        logger.key_errors.load(Ordering::Relaxed)
    );
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    exit(1)
}
