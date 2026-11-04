//! Descant's own settings, in GSettings: whether closing the window stops the
//! music, the volume, the equalizer, and where playback stopped.
//!
//! The Flatpak installs the schema with the app; a run from a checkout uses
//! the copy `build.rs` compiled. Inside the sandbox GLib keeps the values in
//! `~/.var/app/io.github.develonrails.Descant/config/glib-2.0/settings/keyfile`.

use descant_core::{
    equalizer::{Curve, MAX_GAIN_DB},
    quality::Quality,
};
use descant_playback::controller::ResumePoint;
use gtk::{gio, prelude::*};

/// Closing the window hides it and the music plays on. Off by default: an
/// app that keeps running after you closed it, without having said so, is
/// one people find in their process list and distrust.
pub const KEEP_PLAYING: &str = "keep-playing";
/// Descant's own volume, in percent.
pub const VOLUME: &str = "volume";
/// Where playback stopped: (context or "", track, position in ms).
const RESUME: &str = "resume";
pub const EQUALIZER: &str = "equalizer";
/// Which stream to ask Spotify for.
pub const QUALITY: &str = "quality";
const EQUALIZER_GAINS: &str = "equalizer-gains";
const EQUALIZER_PREAMP: &str = "equalizer-preamp";

/// The stream to ask for; anything unrecognised plays at the usual rate.
pub fn quality(settings: &gio::Settings) -> Quality {
    Quality::from_name(&settings.string(QUALITY))
}

/// The saved equalizer curve, whether or not it is switched on.
pub fn equalizer_curve(settings: &gio::Settings) -> Curve {
    let saved: Vec<f64> = settings.get(EQUALIZER_GAINS);
    let mut gains = [0.0; 10];
    for (gain, value) in gains.iter_mut().zip(saved) {
        *gain = value.clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
    }
    Curve {
        preamp: settings.double(EQUALIZER_PREAMP),
        gains,
    }
}

/// What the equalizer plays with: the curve when switched on, else nothing.
pub fn equalizer(settings: &gio::Settings) -> Option<Curve> {
    settings
        .boolean(EQUALIZER)
        .then(|| equalizer_curve(settings))
}

pub fn set_equalizer_curve(settings: &gio::Settings, curve: &Curve) {
    let done = settings
        .set(EQUALIZER_GAINS, curve.gains.to_vec())
        .and_then(|_| settings.set_double(EQUALIZER_PREAMP, curve.preamp));
    if let Err(e) = done {
        log::warn!("equalizer not saved: {e}");
    }
}

/// Where the last run stopped, if anything played.
pub fn resume(settings: &gio::Settings) -> Option<ResumePoint> {
    let (context, track, position_ms) = settings.get::<(String, String, u32)>(RESUME);
    (!track.is_empty()).then(|| ResumePoint {
        context: (!context.is_empty()).then_some(context),
        track,
        position_ms,
    })
}

/// Remembers where playback stands; `None` forgets it (logging out).
pub fn set_resume(settings: &gio::Settings, point: Option<&ResumePoint>) {
    let done = match point {
        Some(p) => settings.set(
            RESUME,
            (
                p.context.clone().unwrap_or_default(),
                p.track.clone(),
                p.position_ms,
            ),
        ),
        None => {
            settings.reset(RESUME);
            Ok(())
        }
    };
    if let Err(e) = done {
        log::warn!("could not remember where playback stopped: {e}");
    }
}

pub fn settings() -> gio::Settings {
    let schema = installed()
        .or_else(built)
        .expect("GSettings schema io.github.develonrails.Descant");
    gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None)
}

fn installed() -> Option<gio::SettingsSchema> {
    gio::SettingsSchemaSource::default()?.lookup(crate::APP_ID, true)
}

/// The schema `build.rs` compiled.
fn built() -> Option<gio::SettingsSchema> {
    let parent = gio::SettingsSchemaSource::default();
    gio::SettingsSchemaSource::from_directory(env!("DESCANT_SCHEMA_DIR"), parent.as_ref(), false)
        .ok()?
        .lookup(crate::APP_ID, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> gio::Settings {
        let schema = built().expect("the compiled schema");
        gio::Settings::new_full(&schema, Some(&gio::memory_settings_backend_new()), None)
    }

    #[test]
    fn closing_quits_and_volume_is_full_until_changed() {
        let settings = in_memory();
        assert!(!settings.boolean(KEEP_PLAYING));
        assert_eq!(settings.int(VOLUME), 100);
    }

    #[test]
    fn volume_is_a_percentage() {
        let key = built().unwrap().key(VOLUME);
        assert!(key.range_check(&0.to_variant()));
        assert!(key.range_check(&100.to_variant()));
        assert!(!key.range_check(&101.to_variant()));
        assert!(!key.range_check(&(-1).to_variant()));
    }

    #[test]
    fn a_choice_sticks() {
        let settings = in_memory();
        settings.set_boolean(KEEP_PLAYING, true).unwrap();
        assert!(settings.boolean(KEEP_PLAYING));
    }

    #[test]
    fn the_equalizer_starts_off_and_flat() {
        let settings = in_memory();
        assert_eq!(equalizer(&settings), None);
        assert!(equalizer_curve(&settings).is_flat());
    }

    #[test]
    fn an_equalizer_curve_is_kept_and_played_once_on() {
        let settings = in_memory();
        let mut gains = [0.0; 10];
        gains[3] = 4.5;
        let curve = Curve {
            preamp: -2.0,
            gains,
        };
        set_equalizer_curve(&settings, &curve);
        assert_eq!(equalizer(&settings), None);
        settings.set_boolean(EQUALIZER, true).unwrap();
        assert_eq!(equalizer(&settings), Some(curve));
    }

    #[test]
    fn nothing_to_resume_until_something_played() {
        assert_eq!(resume(&in_memory()), None);
    }

    #[test]
    fn the_place_is_remembered_and_forgotten() {
        let settings = in_memory();
        let point = ResumePoint {
            context: Some("spotify:playlist:x".into()),
            track: "spotify:track:a".into(),
            position_ms: 83_000,
        };
        set_resume(&settings, Some(&point));
        assert_eq!(resume(&settings), Some(point));
        set_resume(&settings, None);
        assert_eq!(resume(&settings), None);
    }

    #[test]
    fn a_track_without_context_comes_back_without_one() {
        let settings = in_memory();
        let point = ResumePoint {
            context: None,
            track: "spotify:track:a".into(),
            position_ms: 0,
        };
        set_resume(&settings, Some(&point));
        assert_eq!(resume(&settings), Some(point));
    }
}
