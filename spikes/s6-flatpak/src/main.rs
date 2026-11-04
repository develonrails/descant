//! Spike S6 — does the whole chain work inside a Flatpak?
//!
//!   descant-spike                 opens a libadwaita window
//!   descant-spike --gui-smoke     opens the window and quits after two seconds
//!   descant-spike --play-test S   no window: plays S seconds of a track through
//!                               librespot's PulseAudio backend, using the
//!                               credentials stored by spikes/s1-playback
//!                               ($DESCANT_SPIKE_CACHE)

use std::{env, process::ExitCode, time::Duration};

use adw::prelude::*;
use librespot_core::{cache::Cache, config::SessionConfig, session::Session, spotify_uri::SpotifyUri};
use librespot_metadata::{Metadata, Playlist};
use librespot_playback::{
    audio_backend,
    config::{AudioFormat, PlayerConfig},
    mixer::NoOpVolume,
    player::{Player, PlayerEvent},
};

const APP_ID: &str = "io.github.develonrails.DescantSpike";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--play-test") {
        let secs = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(5.0);
        return tokio::runtime::Runtime::new().unwrap().block_on(play_test(secs));
    }
    let smoke = args.iter().any(|a| a == "--gui-smoke");

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| {
        let label = gtk::Label::builder()
            .label("Descant S6: GTK4 + libadwaita in a Flatpak")
            .css_classes(["title-2"])
            .vexpand(true)
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&label));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Descant S6")
            .default_width(480)
            .default_height(240)
            .content(&view)
            .build();
        window.present();
        println!("GUI_OK gtk {}.{}.{} adw {}.{}.{}",
            gtk::major_version(), gtk::minor_version(), gtk::micro_version(),
            adw::major_version(), adw::minor_version(), adw::micro_version());
        if smoke {
            let app = app.clone();
            gtk::glib::timeout_add_seconds_local_once(2, move || app.quit());
        }
    });
    // GTK must not see our own flags.
    app.run_with_args(&args[..1]);
    ExitCode::SUCCESS
}

async fn play_test(secs: f64) -> ExitCode {
    let Some(dir) = env::var_os("DESCANT_SPIKE_CACHE") else {
        eprintln!("PLAY_FAIL DESCANT_SPIKE_CACHE not set");
        return ExitCode::FAILURE;
    };
    let cache = match Cache::new(Some(dir), None, None, None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("PLAY_FAIL cache: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(creds) = cache.credentials() else {
        eprintln!("PLAY_FAIL no credentials in cache");
        return ExitCode::FAILURE;
    };
    let session = Session::new(SessionConfig::default(), Some(cache));
    if let Err(e) = session.connect(creds, false).await {
        eprintln!("PLAY_FAIL connect: {e}");
        return ExitCode::FAILURE;
    }
    let playlist = SpotifyUri::from_uri("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M").unwrap();
    let track = match Playlist::get(&session, &playlist).await {
        Ok(p) => p.tracks().next().cloned(),
        Err(e) => {
            eprintln!("PLAY_FAIL playlist: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(track) = track else {
        eprintln!("PLAY_FAIL empty playlist");
        return ExitCode::FAILURE;
    };

    let Some(backend) = audio_backend::find(Some("pulseaudio".to_string())) else {
        eprintln!("PLAY_FAIL pulseaudio backend missing");
        return ExitCode::FAILURE;
    };
    let player = Player::new(PlayerConfig::default(), session, Box::new(NoOpVolume), move || {
        backend(None, AudioFormat::default())
    });
    let mut events = player.get_player_event_channel();
    player.load(track.clone(), true, 0);

    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(secs);
    let mut playing = false;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(PlayerEvent::Playing { .. })) => {
                playing = true;
                println!("PLAYING {}", track.to_uri());
            }
            Ok(Some(PlayerEvent::Unavailable { .. })) => {
                eprintln!("PLAY_FAIL track unavailable");
                return ExitCode::FAILURE;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    player.stop();
    tokio::time::sleep(Duration::from_millis(300)).await;
    if playing {
        println!("PLAY_OK");
        ExitCode::SUCCESS
    } else {
        eprintln!("PLAY_FAIL never started");
        ExitCode::FAILURE
    }
}
