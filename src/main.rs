//! Descant: a small, fast, native GNOME client for Spotify.

mod artist;
mod grid;
mod home;
mod images;
mod mpris;
mod nav;
mod preferences;
mod qr;
mod search;
mod settings;
mod tracks;
mod widgets;
mod window;

use std::sync::OnceLock;

use adw::prelude::*;

pub const APP_ID: &str = "io.github.develonrails.Descant";

/// librespot, the keyring and HTTP run on a small tokio runtime beside GTK's
/// main loop. Two workers: playback must never wait behind a slow request.
/// Two threads for blocking work (reading and decoding covers) too: tokio
/// would otherwise start one per cover in a grid, each keeping a malloc
/// arena of its own.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(2)
            .thread_name("descant-io")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

/// Rounded covers, round portraits, the playing track accented.
const CSS: &str = "
.cover { border-radius: 6px; }
.cover-large { border-radius: 12px; }
.portrait { border-radius: 9999px; }
.track-row.playing .title { color: var(--accent-color); font-weight: bold; }
.liked-tile {
  background: linear-gradient(135deg, #4a3aff, #b6a5ff);
  color: white;
  border-radius: 6px;
}
.home-tile { padding: 0; min-height: 56px; }
.progress-edge { min-height: 3px; padding: 0; margin: 0; }
.progress-edge trough { min-height: 3px; border-radius: 0; padding: 0; margin: 0; }
.progress-edge highlight { border-radius: 0; }
.progress-edge slider { min-width: 0; min-height: 0; margin: 0; background: none; border: none; box-shadow: none; }
.progress-edge:hover trough { min-height: 5px; }
.progress-edge:hover slider {
  min-width: 11px;
  min-height: 11px;
  margin: -3px;
  border-radius: 9999px;
  background: var(--accent-color);
}
";

/// The icons Adwaita lacks (the hearts), built into the binary. GTK finds
/// them on its own under the app's resource path, the app id as a path.
fn register_resources() {
    gtk::gio::resources_register_include!("descant.gresource").expect("built-in resources");
}

fn main() -> gtk::glib::ExitCode {
    descant_playback::tap::install();
    register_resources();
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });
    app.connect_activate(window::activate);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("win.preferences", &["<Control>comma"]);
    app.set_accels_for_action("win.shortcuts", &["<Control>question"]);
    app.set_accels_for_action("win.play-pause", &["<Control>space"]);
    app.set_accels_for_action("win.next", &["<Control>Right"]);
    app.set_accels_for_action("win.previous", &["<Control>Left"]);
    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hearts_are_built_in_where_gtk_looks_for_icons() {
        register_resources();
        let base = APP_ID.replace('.', "/");
        for icon in [widgets::LIKED_ICON, widgets::LIKE_ICON] {
            let path = format!("/{base}/icons/scalable/actions/{icon}.svg");
            let found = gtk::gio::resources_lookup_data(&path, gtk::gio::ResourceLookupFlags::NONE);
            assert!(found.is_ok(), "{path} is not built in");
        }
    }
}
