//! The Preferences dialog: the account, how Descant runs, what it keeps on
//! disk, and the equalizer.
//!
//! Everything is a GSettings key. The equalizer also changes the curve the
//! audio path plays with at once, and saves it a moment after the last move
//! of a slider rather than on every step of a drag.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use adw::prelude::*;
use descant_core::{
    equalizer::{BANDS, Curve, MAX_GAIN_DB, PRESETS, preset_named},
    quality::Quality,
    supervisor::Command,
};
use descant_playback::equalizer::EqualizerSetting;
use gtk::{gio, glib};

use crate::{
    images,
    nav::{Event, Nav},
    runtime,
    settings::{self, EQUALIZER, KEEP_PLAYING, QUALITY},
};

/// `nav` is there once signed in; without it there is no account to show.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    settings: &gio::Settings,
    eq: &EqualizerSetting,
    nav: Option<&Nav>,
) {
    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        .build();
    dialog.add(&general(settings, &dialog, nav));
    dialog.add(&Equalizer::page(settings, eq));
    dialog.present(Some(parent));
}

fn general(
    settings: &gio::Settings,
    dialog: &adw::PreferencesDialog,
    nav: Option<&Nav>,
) -> adw::PreferencesPage {
    let keep_playing = adw::SwitchRow::builder()
        .title("Keep playing when the window is closed")
        .subtitle(
            "The music and Spotify Connect carry on. Quit from the main menu to stop Descant.",
        )
        .build();
    settings.bind(KEEP_PLAYING, &keep_playing, "active").build();
    let running = adw::PreferencesGroup::builder().title("Running").build();
    running.add(&keep_playing);

    let playback = adw::PreferencesGroup::builder().title("Playback").build();
    playback.add(&quality_row(settings, nav));

    let storage = adw::PreferencesGroup::builder().title("Storage").build();
    storage.add(&artwork_row(dialog));
    storage.add(
        &adw::ActionRow::builder()
            .title("Audio")
            .subtitle("Not kept: tracks are streamed and never stored.")
            .build(),
    );
    storage.add(
        &adw::ActionRow::builder()
            .title("Sign-in")
            .subtitle("Kept in the system keyring until you log out.")
            .build(),
    );

    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("emblem-system-symbolic")
        .build();
    if let Some(nav) = nav {
        let account = adw::PreferencesGroup::builder().title("Account").build();
        account.add(&account_row(nav, dialog));
        page.add(&account);
    }
    page.add(&playback);
    page.add(&running);
    page.add(&storage);
    page
}

/// Which of Spotify's three streams to ask for. The bitrate is fixed when
/// the player is built, so picking another one reconnects; playback carries
/// on from where it was, after a pause of about a second.
fn quality_row(settings: &gio::Settings, nav: Option<&Nav>) -> adw::ComboRow {
    let names = gtk::StringList::new(&[
        "Normal — 96 kbps",
        "High — 160 kbps",
        "Very high — 320 kbps",
    ]);
    let chosen = settings::quality(settings);
    let row = adw::ComboRow::builder()
        .title("Streaming Quality")
        .subtitle("Very high needs Premium. Changing this reconnects.")
        .model(&names)
        .selected(
            Quality::ALL
                .iter()
                .position(|quality| *quality == chosen)
                .unwrap_or_default() as u32,
        )
        .build();
    let settings = settings.clone();
    let nav = nav.cloned();
    row.connect_selected_notify(move |row| {
        let Some(quality) = Quality::ALL.get(row.selected() as usize).copied() else {
            return;
        };
        if settings::quality(&settings) == quality {
            return;
        }
        if let Err(e) = settings.set_string(QUALITY, quality.as_str()) {
            log::warn!("streaming quality not saved: {e}");
            return;
        }
        // Without a session there is no player to rebuild; the next start
        // reads the setting anyway.
        if let Some(nav) = &nav {
            nav.send(Command::SetQuality(quality));
        }
    });
    row
}

/// Who is signed in (the display name, else the account id), and Log Out.
fn account_row(nav: &Nav, dialog: &adw::PreferencesDialog) -> adw::ActionRow {
    let avatar = adw::Avatar::new(32, None, false);
    let row = adw::ActionRow::builder()
        .title(nav.session().username())
        .subtitle("Signed in to Spotify")
        .build();
    row.add_prefix(&avatar);
    let log_out = gtk::Button::builder()
        .label("Log Out")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "destructive-action"])
        .build();
    row.add_suffix(&log_out);
    let (n, weak) = (nav.clone(), dialog.downgrade());
    log_out.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
        n.tell(Event::LogOut);
    });

    // The profile is asked for once per session, when Preferences opens.
    let show = {
        let (row, avatar) = (row.downgrade(), avatar.downgrade());
        move |name: &Option<String>| {
            if let (Some(name), Some(row), Some(avatar)) = (name, row.upgrade(), avatar.upgrade()) {
                row.set_title(name);
                avatar.set_text(Some(name));
                avatar.set_show_initials(true);
            }
        }
    };
    let known = nav.library.profile.borrow().clone();
    match known {
        Some(profile) => show(&profile.name),
        None => {
            let n = nav.clone();
            glib::spawn_future_local(async move {
                let session = n.session();
                let asked = runtime()
                    .spawn(async move { descant_spotify::profile::me(&session).await })
                    .await;
                if let Ok(Ok(profile)) = asked {
                    show(&profile.name);
                    *n.library.profile.borrow_mut() = Some(profile);
                }
            });
        }
    }
    row
}

/// The cover cache: where it is, how big, and a button to empty it.
fn artwork_row(dialog: &adw::PreferencesDialog) -> adw::ActionRow {
    let place = home_relative(&images::cache_dir());
    let row = adw::ActionRow::builder()
        .title("Artwork cache")
        .subtitle(&place)
        .build();
    let clear = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("user-trash-symbolic")
                .label("Clear")
                .build(),
        )
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    row.add_suffix(&clear);

    let (r, p) = (row.clone(), place.clone());
    glib::spawn_future_local(async move {
        r.set_subtitle(&format!("{} in {p}", size(images::disk_usage().await)));
    });
    // Weak references: the row owns the button that owns this closure.
    let (weak_dialog, weak_row) = (dialog.downgrade(), row.downgrade());
    clear.connect_clicked(move |button| {
        button.set_sensitive(false);
        let (weak_dialog, weak_row, place) = (weak_dialog.clone(), weak_row.clone(), place.clone());
        glib::spawn_future_local(async move {
            images::clear().await;
            if let Some(row) = weak_row.upgrade() {
                row.set_subtitle(&format!("Empty, in {place}"));
            }
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.add_toast(adw::Toast::new("Artwork cleared"));
            }
        });
    });
    row
}

/// The equalizer page and the state its controls share.
struct Equalizer {
    settings: gio::Settings,
    setting: EqualizerSetting,
    enabled: adw::SwitchRow,
    preset: adw::ComboRow,
    /// The pre-amp, then the ten bands.
    sliders: Vec<gtk::Scale>,
    /// Set while controls are moved from code, so they do not echo back.
    following: Cell<bool>,
    save_pending: RefCell<Option<glib::SourceId>>,
}

impl Equalizer {
    fn page(settings: &gio::Settings, setting: &EqualizerSetting) -> adw::PreferencesPage {
        let curve = settings::equalizer_curve(settings);
        let enabled = adw::SwitchRow::builder()
            .title("Equalizer")
            .subtitle("Ten bands, for what plays on this computer. Other devices are not affected.")
            .active(settings.boolean(EQUALIZER))
            .build();
        let mut names: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
        names.push("Custom");
        let preset = adw::ComboRow::builder()
            .title("Preset")
            .model(&gtk::StringList::new(&names))
            .selected(preset_index(&curve.gains))
            .build();
        let sliders: Vec<gtk::Scale> = std::iter::once(curve.preamp)
            .chain(curve.gains)
            .map(slider)
            .collect();

        let bands = gtk::Box::builder()
            .homogeneous(true)
            .spacing(4)
            .margin_top(12)
            .margin_bottom(12)
            .css_classes(["card"])
            .build();
        let labels = std::iter::once("Pre".to_string()).chain(BANDS.iter().map(|f| band_label(*f)));
        for (scale, label) in sliders.iter().zip(labels) {
            let column = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .margin_top(12)
                .margin_bottom(12)
                .build();
            column.append(scale);
            column.append(
                &gtk::Label::builder()
                    .label(label)
                    .css_classes(["caption", "dim-label", "numeric"])
                    .build(),
            );
            bands.append(&column);
        }

        let group = adw::PreferencesGroup::new();
        group.add(&enabled);
        group.add(&preset);
        group.add(&bands);
        let page = adw::PreferencesPage::builder()
            .title("Equalizer")
            .icon_name("audio-speakers-symbolic")
            .build();
        page.add(&group);

        let eq = Rc::new(Equalizer {
            settings: settings.clone(),
            setting: setting.clone(),
            enabled,
            preset,
            sliders,
            following: Cell::new(false),
            save_pending: RefCell::new(None),
        });
        eq.show_enabled();
        let e = eq.clone();
        eq.enabled.connect_active_notify(move |row| {
            let _ = e.settings.set_boolean(EQUALIZER, row.is_active());
            e.show_enabled();
            e.apply();
        });
        let e = eq.clone();
        eq.preset
            .connect_selected_notify(move |row| e.preset_picked(row.selected()));
        for scale in &eq.sliders {
            let e = eq.clone();
            scale.connect_value_changed(move |_| e.slider_moved());
        }
        page
    }

    fn curve(&self) -> Curve {
        let mut values = self.sliders.iter().map(|s| s.value());
        let preamp = values.next().unwrap_or(0.0);
        let mut gains = [0.0; 10];
        for (gain, value) in gains.iter_mut().zip(values) {
            *gain = value;
        }
        Curve { preamp, gains }
    }

    fn show_enabled(&self) {
        let on = self.enabled.is_active();
        self.preset.set_sensitive(on);
        for scale in &self.sliders {
            scale.set_sensitive(on);
        }
    }

    /// The audio path follows at once; the settings a moment later.
    fn apply(self: &Rc<Self>) {
        let curve = self.curve();
        if let Ok(mut playing) = self.setting.lock() {
            *playing = self.enabled.is_active().then_some(curve);
        }
        if let Some(pending) = self.save_pending.borrow_mut().take() {
            pending.remove();
        }
        let eq = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(400), move || {
            eq.save_pending.borrow_mut().take();
            settings::set_equalizer_curve(&eq.settings, &eq.curve());
        });
        *self.save_pending.borrow_mut() = Some(id);
    }

    fn preset_picked(self: &Rc<Self>, index: u32) {
        let Some(preset) = PRESETS.get(index as usize) else {
            return; // "Custom" is where moved sliders land, not a curve
        };
        if self.following.get() {
            return;
        }
        self.following.set(true);
        for (scale, gain) in self.sliders.iter().skip(1).zip(preset.gains) {
            scale.set_value(gain);
        }
        self.following.set(false);
        self.apply();
    }

    fn slider_moved(self: &Rc<Self>) {
        for scale in &self.sliders {
            scale.set_tooltip_text(Some(&format!("{:+.1} dB", scale.value())));
        }
        if self.following.get() {
            return;
        }
        self.following.set(true);
        self.preset.set_selected(preset_index(&self.curve().gains));
        self.following.set(false);
        self.apply();
    }
}

/// A vertical slider from −12 to +12 dB, up is louder.
fn slider(value: f64) -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Vertical, -MAX_GAIN_DB, MAX_GAIN_DB, 0.5);
    scale.set_inverted(true);
    scale.set_value(value);
    scale.set_height_request(160);
    scale.set_halign(gtk::Align::Center);
    scale.add_mark(0.0, gtk::PositionType::Right, None);
    scale.set_tooltip_text(Some(&format!("{value:+.1} dB")));
    scale
}

/// The row of a preset in the list; past the end ("Custom") for others.
fn preset_index(gains: &[f64; 10]) -> u32 {
    let index = preset_named(gains)
        .and_then(|name| PRESETS.iter().position(|p| p.name == name))
        .unwrap_or(PRESETS.len());
    index as u32
}

/// "60", "1K", "16K".
fn band_label(freq: f64) -> String {
    if freq >= 1_000.0 {
        format!("{}K", freq / 1_000.0)
    } else {
        format!("{freq}")
    }
}

fn size(bytes: u64) -> String {
    match bytes {
        0 => "Empty".to_string(),
        b if b < 1024 * 1024 => format!("{} KB", b.div_ceil(1024)),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// A path with the home directory written as `~`.
fn home_relative(path: &std::path::Path) -> String {
    let home = glib::home_dir();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_are_labelled_like_a_hifi() {
        let labels: Vec<String> = BANDS.iter().map(|f| band_label(*f)).collect();
        assert_eq!(
            labels,
            [
                "60", "170", "310", "600", "1K", "3K", "6K", "12K", "14K", "16K"
            ]
        );
    }

    #[test]
    fn sizes_read_easily() {
        assert_eq!(size(0), "Empty");
        assert_eq!(size(1_500), "2 KB");
        assert_eq!(size(12 * 1024 * 1024 + 300 * 1024), "12.3 MB");
    }

    #[test]
    fn a_curve_that_is_no_preset_is_custom() {
        assert_eq!(preset_index(&[0.0; 10]), 0);
        assert_eq!(preset_index(&[1.0; 10]), PRESETS.len() as u32);
    }
}
