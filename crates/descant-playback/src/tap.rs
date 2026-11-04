//! Descant's logger, which also listens for audio-key refusals.
//!
//! librespot reports a refused audio key only as a log line
//! (`error audio key 0 2`) and then carries on without decryption. The tap is
//! the process-wide logger: it prints what is worth printing and hands every
//! refusal to the engine as a [`KeyError`].

use std::sync::{Mutex, OnceLock};

use descant_core::key::KeyError;
use log::{Level, LevelFilter, Log, Metadata, Record};

pub type OnKeyRefused = Box<dyn Fn(KeyError) + Send + Sync>;

/// What to do with one log record.
#[derive(Debug, PartialEq, Eq)]
pub struct Routing {
    pub print: bool,
    pub key: Option<KeyError>,
}

/// Decides, per record, whether to print it and whether it is a refusal.
/// `verbose` prints librespot's info and debug lines as well.
pub fn route(level: Level, target: &str, message: &str, verbose: bool) -> Routing {
    let key = if is_key_line(level, target) {
        KeyError::from_log_line(message)
    } else {
        None
    };
    Routing {
        print: printable(level, target, verbose),
        key,
    }
}

fn is_key_line(level: Level, target: &str) -> bool {
    level == Level::Error && target.starts_with("librespot_core::audio_key")
}

fn printable(level: Level, target: &str, verbose: bool) -> bool {
    if target.starts_with("symphonia") {
        false
    } else if target.starts_with("descant") {
        level <= Level::Info || verbose
    } else {
        level <= Level::Warn || verbose
    }
}

struct Tap {
    verbose: bool,
    /// Replaced by every new controller (after logging out and in again,
    /// refusals must reach the new one, not the old channel).
    on_key: Mutex<Option<OnKeyRefused>>,
}

static TAP: OnceLock<Tap> = OnceLock::new();

/// Installs the tap as the process logger. `DESCANT_LOG=debug` makes it verbose.
/// Safe to call more than once; later calls change nothing.
pub fn install() {
    let tap = TAP.get_or_init(|| Tap {
        verbose: std::env::var("DESCANT_LOG").is_ok_and(|v| v == "debug"),
        on_key: Mutex::new(None),
    });
    if log::set_logger(tap).is_ok() {
        log::set_max_level(if tap.verbose {
            LevelFilter::Debug
        } else {
            LevelFilter::Info
        });
    }
}

/// Where refusals go from now on; the controller sets this when it starts.
pub fn on_key_refused(callback: OnKeyRefused) {
    install();
    if let Some(tap) = TAP.get() {
        *tap.on_key.lock().unwrap_or_else(|e| e.into_inner()) = Some(callback);
    }
}

impl Log for Tap {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Debug
    }

    fn log(&self, record: &Record) {
        let (level, target) = (record.level(), record.target());
        // Most records are neither printed nor a refusal: don't format those.
        if !printable(level, target, self.verbose) && !is_key_line(level, target) {
            return;
        }
        let message = record.args().to_string();
        let routing = route(level, target, &message, self.verbose);
        if let Some(key) = routing.key
            && let Some(callback) = &*self.on_key.lock().unwrap_or_else(|e| e.into_inner())
        {
            callback(key);
        }
        if routing.print {
            eprintln!("{:<5} {}: {}", record.level(), record.target(), message);
        }
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_key_is_caught_and_printed() {
        let r = route(
            Level::Error,
            "librespot_core::audio_key",
            "error audio key 0 2",
            false,
        );
        assert_eq!(
            r,
            Routing {
                print: true,
                key: Some(KeyError::RateLimited)
            }
        );
    }

    #[test]
    fn only_the_audio_key_module_counts() {
        let r = route(Level::Error, "somewhere_else", "error audio key 0 2", false);
        assert_eq!(r.key, None);
    }

    #[test]
    fn symphonias_junk_skipping_is_never_printed() {
        // garbage input makes the mp3 demuxer log hundreds of these
        let r = route(
            Level::Warn,
            "symphonia_bundle_mp3::demuxer",
            "skipping junk at 18026 bytes",
            true,
        );
        assert_eq!(
            r,
            Routing {
                print: false,
                key: None
            }
        );
    }

    #[test]
    fn librespot_chatter_only_when_verbose() {
        let line = "Loading <Song> with Spotify URI <spotify:track:x>";
        assert!(!route(Level::Info, "librespot_playback::player", line, false).print);
        assert!(route(Level::Info, "librespot_playback::player", line, true).print);
        assert!(route(Level::Warn, "librespot_playback::player", "warn", false).print);
    }

    #[test]
    fn descant_info_is_always_printed() {
        assert!(route(Level::Info, "descant_playback::engine", "connected", false).print);
        assert!(!route(Level::Debug, "descant_playback::engine", "detail", false).print);
        assert!(route(Level::Debug, "descant_playback::engine", "detail", true).print);
    }
}
