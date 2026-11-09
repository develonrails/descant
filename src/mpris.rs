//! Media keys and GNOME's media controls, over MPRIS.
//!
//! Descant owns `org.mpris.MediaPlayer2.Descant` on the session bus: the shell's
//! media controls, keyboard media keys and headset buttons all talk to it.
//! It is served on the D-Bus connection GApplication already has, through
//! GDBus: no second connection, and no runtime or thread of its own.

use std::{cell::RefCell, rc::Rc};

use descant_core::{
    supervisor::{Repeat, Status},
    track::TrackInfo,
};
use gtk::{gio, glib, prelude::*};

const BUS_NAME: &str = "org.mpris.MediaPlayer2.Descant";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// The two interfaces, as much of them as Descant does. Opening URIs is
/// declared (the spec asks for it) but not offered.
const INTERFACES: &str = r#"<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
    <property name="DesktopEntry" type="s" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek"><arg direction="in" name="Offset" type="x"/></method>
    <method name="SetPosition">
      <arg direction="in" name="TrackId" type="o"/>
      <arg direction="in" name="Position" type="x"/>
    </method>
    <method name="OpenUri"><arg direction="in" name="Uri" type="s"/></method>
    <signal name="Seeked"><arg name="Position" type="x"/></signal>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="LoopStatus" type="s" access="readwrite"/>
    <property name="Rate" type="d" access="read"/>
    <property name="Shuffle" type="b" access="readwrite"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="readwrite"/>
    <property name="Position" type="x" access="read"/>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>"#;

/// The Player properties announced when they change. Position is not one of
/// them: MPRIS clients run it on by themselves and listen for `Seeked`.
const SIGNALLED: [&str; 10] = [
    "PlaybackStatus",
    "LoopStatus",
    "Shuffle",
    "Metadata",
    "Volume",
    "CanGoNext",
    "CanGoPrevious",
    "CanPlay",
    "CanPause",
    "CanSeek",
];

/// What the MPRIS buttons do, and where the place in the track comes from.
pub struct Actions {
    pub play_pause: Box<dyn Fn()>,
    pub play: Box<dyn Fn()>,
    pub pause: Box<dyn Fn()>,
    pub next: Box<dyn Fn()>,
    pub previous: Box<dyn Fn()>,
    /// Go to this place in the playing track, in ms.
    pub seek: Box<dyn Fn(u32)>,
    /// Set the volume, in percent.
    pub volume: Box<dyn Fn(f64)>,
    pub shuffle: Box<dyn Fn(bool)>,
    pub repeat: Box<dyn Fn(Repeat)>,
    pub raise: Box<dyn Fn()>,
    pub quit: Box<dyn Fn()>,
    /// Where the playing track stands now, in ms.
    pub position: Box<dyn Fn() -> u32>,
}

/// The track in the bar, as far as seeking in it needs.
struct Shown {
    /// Its MPRIS track id; local files have none.
    id: Option<String>,
    duration_ms: u32,
}

struct State {
    status: Status,
    track: Option<Shown>,
    metadata: glib::Variant,
    /// 0.0–1.0, as MPRIS has it.
    volume: f64,
    shuffle: bool,
    repeat: Repeat,
}

impl State {
    fn new() -> State {
        State {
            status: Status::Idle,
            track: None,
            metadata: glib::VariantDict::new(None).end(),
            volume: 1.0,
            shuffle: false,
            repeat: Repeat::Off,
        }
    }

    fn duration_ms(&self) -> u32 {
        self.track.as_ref().map_or(0, |t| t.duration_ms)
    }

    fn controls(&self) -> Controls {
        controls(&self.status, self.track.is_some(), self.duration_ms())
    }

    /// The track's length, when it can be sought in.
    fn seekable(&self) -> Option<u32> {
        self.controls().seek.then(|| self.duration_ms())
    }

    fn track_id(&self) -> Option<&str> {
        self.track.as_ref()?.id.as_deref()
    }
}

/// What the bus callbacks share. They borrow the state only to decide, never
/// while Descant acts: acting comes back here to announce what changed.
struct Shared {
    actions: Actions,
    state: RefCell<State>,
}

impl Shared {
    fn call(&self, method: &str, parameters: &glib::Variant) {
        let actions = &self.actions;
        match method {
            "Raise" => (actions.raise)(),
            "Quit" => (actions.quit)(),
            "Next" => (actions.next)(),
            "Previous" => (actions.previous)(),
            "Pause" | "Stop" => (actions.pause)(),
            "PlayPause" => (actions.play_pause)(),
            "Play" => (actions.play)(),
            "Seek" => {
                let length = self.state.borrow().seekable();
                if let (Some(length), Some(offset)) =
                    (length, parameters.child_value(0).get::<i64>())
                {
                    match seek_by((actions.position)(), offset, length) {
                        Seek::To(ms) => (actions.seek)(ms),
                        Seek::Next => (actions.next)(),
                    }
                }
            }
            "SetPosition" => {
                let (id, position) = (parameters.child_value(0), parameters.child_value(1));
                let target = {
                    let state = self.state.borrow();
                    match (state.seekable(), id.str(), position.get::<i64>()) {
                        (Some(length), Some(id), Some(us)) => {
                            set_position(state.track_id(), id, us, length)
                        }
                        _ => None,
                    }
                };
                if let Some(ms) = target {
                    (actions.seek)(ms);
                }
            }
            _ => {} // OpenUri: not offered
        }
    }

    fn get(&self, name: &str) -> glib::Variant {
        let position = if name == "Position" {
            (self.actions.position)()
        } else {
            0
        };
        property(name, &self.state.borrow(), position)
    }

    fn set(&self, name: &str, value: &glib::Variant) -> bool {
        let actions = &self.actions;
        match name {
            "Volume" => value
                .get::<f64>()
                .and_then(volume_percent)
                .map(|percent| (actions.volume)(percent))
                .is_some(),
            "Shuffle" => value
                .get::<bool>()
                .map(|on| (actions.shuffle)(on))
                .is_some(),
            "LoopStatus" => value
                .str()
                .and_then(repeat_from)
                .map(|mode| (actions.repeat)(mode))
                .is_some(),
            _ => false,
        }
    }
}

pub struct Mpris {
    connection: gio::DBusConnection,
    shared: Rc<Shared>,
    objects: Vec<gio::RegistrationId>,
    name: Option<gio::OwnerId>,
}

impl Mpris {
    /// Serves MPRIS on the application's bus connection. Descant works
    /// without it, so a failure is only logged.
    pub fn start(app: &impl IsA<gio::Application>, actions: Actions) -> Option<Mpris> {
        let Some(connection) = app.as_ref().dbus_connection() else {
            log::warn!("MPRIS unavailable: no session bus connection");
            return None;
        };
        let node = gio::DBusNodeInfo::for_xml(INTERFACES)
            .inspect_err(|e| log::warn!("MPRIS unavailable: {e}"))
            .ok()?;
        let shared = Rc::new(Shared {
            actions,
            state: RefCell::new(State::new()),
        });
        // Made first, so a failure halfway gives back what was registered.
        let mut mpris = Mpris {
            connection: connection.clone(),
            shared: shared.clone(),
            objects: Vec::new(),
            name: None,
        };
        for interface in node.interfaces() {
            let (call, get, set) = (shared.clone(), shared.clone(), shared.clone());
            let registered = connection
                .register_object(PATH, interface)
                .method_call(move |_, _, _, _, method, parameters, invocation| {
                    call.call(method, &parameters);
                    invocation.return_value(None);
                })
                .property(move |_, _, _, _, name| get.get(name))
                .set_property(move |_, _, _, _, name, value| set.set(name, &value))
                .build();
            match registered {
                Ok(object) => mpris.objects.push(object),
                Err(e) => {
                    log::warn!("MPRIS unavailable: {e}");
                    return None;
                }
            }
        }
        mpris.name = Some(gio::bus_own_name_on_connection(
            &connection,
            BUS_NAME,
            gio::BusNameOwnerFlags::NONE,
            |_, _| {},
            |_, name| log::warn!("could not own {name}; media keys will not reach Descant"),
        ));
        Some(mpris)
    }

    pub fn status(&self, status: &Status) {
        self.update(|state| state.status = status.clone());
    }

    pub fn track(&self, info: &TrackInfo) {
        self.update(|state| {
            state.metadata = metadata(info);
            state.track = Some(Shown {
                id: track_id(&info.uri),
                duration_ms: info.duration_ms,
            });
        });
    }

    /// Nothing in the bar any more: logged out.
    pub fn clear(&self) {
        self.update(|state| {
            state.status = Status::Idle;
            state.track = None;
            state.metadata = glib::VariantDict::new(None).end();
        });
    }

    pub fn volume(&self, percent: f64) {
        self.update(|state| state.volume = (percent / 100.0).clamp(0.0, 1.0));
    }

    pub fn shuffle(&self, on: bool) {
        self.update(|state| state.shuffle = on);
    }

    pub fn repeat(&self, mode: Repeat) {
        self.update(|state| state.repeat = mode);
    }

    /// The engine named a new place: tells clients if it is a jump rather
    /// than playback running on. `expected` is where it was thought to be.
    pub fn moved(&self, expected: Option<(&str, u32)>, track: &str, position_ms: u32) {
        if !jumped(expected, track, position_ms) {
            return;
        }
        let parameters = (i64::from(position_ms) * 1000,).to_variant();
        if let Err(e) = self
            .connection
            .emit_signal(None, PATH, PLAYER, "Seeked", Some(&parameters))
        {
            log::debug!("MPRIS signal not sent: {e}");
        }
    }

    /// Changes the state, then tells listeners which properties that changed.
    fn update(&self, change: impl FnOnce(&mut State)) {
        let before = signalled(&self.shared.state.borrow());
        change(&mut self.shared.state.borrow_mut());
        let after = signalled(&self.shared.state.borrow());
        let changed = changes(&before, &after);
        if changed.is_empty() {
            return;
        }
        let dict = glib::VariantDict::new(None);
        for (name, value) in changed {
            dict.insert_value(name, &value);
        }
        let parameters = glib::Variant::tuple_from_iter([
            PLAYER.to_variant(),
            dict.end(),
            Vec::<String>::new().to_variant(),
        ]);
        if let Err(e) = self.connection.emit_signal(
            None,
            PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Some(&parameters),
        ) {
            log::debug!("MPRIS signal not sent: {e}");
        }
    }
}

/// Gives the bus name and the objects back, so a player that is gone does not
/// keep the connection alive.
impl Drop for Mpris {
    fn drop(&mut self) {
        if let Some(name) = self.name.take() {
            gio::bus_unown_name(name);
        }
        for object in self.objects.drain(..) {
            let _ = self.connection.unregister_object(object);
        }
    }
}

/// `position_ms` is only read for `Position`.
fn property(name: &str, state: &State, position_ms: u32) -> glib::Variant {
    let controls = state.controls();
    match name {
        "Identity" => "Descant".to_variant(),
        "DesktopEntry" => crate::APP_ID.to_variant(),
        "SupportedUriSchemes" | "SupportedMimeTypes" => Vec::<String>::new().to_variant(),
        "PlaybackStatus" => playback_status(&state.status).to_variant(),
        "LoopStatus" => loop_status(state.repeat).to_variant(),
        "Shuffle" => state.shuffle.to_variant(),
        "Metadata" => state.metadata.clone(),
        "Volume" => state.volume.to_variant(),
        "Position" => (i64::from(position_ms.min(state.duration_ms())) * 1000).to_variant(),
        "Rate" | "MinimumRate" | "MaximumRate" => 1.0f64.to_variant(),
        "CanGoNext" | "CanGoPrevious" | "CanPlay" | "CanPause" => controls.transport.to_variant(),
        "CanSeek" => controls.seek.to_variant(),
        "HasTrackList" => false.to_variant(),
        // CanQuit, CanRaise, CanControl
        _ => true.to_variant(),
    }
}

fn signalled(state: &State) -> Vec<glib::Variant> {
    SIGNALLED
        .iter()
        .map(|name| property(name, state, 0))
        .collect()
}

/// The signalled properties whose value differs, with their new value.
fn changes(
    before: &[glib::Variant],
    after: &[glib::Variant],
) -> Vec<(&'static str, glib::Variant)> {
    SIGNALLED
        .iter()
        .zip(before.iter().zip(after))
        .filter(|(_, (was, is))| was != is)
        .map(|(name, (_, is))| (*name, is.clone()))
        .collect()
}

/// Which controls do something now. Nothing does before a track is in the
/// bar (or after logging out), or on an account that cannot play here; a
/// track of unknown length cannot be sought in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Controls {
    /// Play, pause, next and previous.
    transport: bool,
    seek: bool,
}

fn controls(status: &Status, has_track: bool, duration_ms: u32) -> Controls {
    if !has_track || matches!(status, Status::Blocked) {
        return Controls::default();
    }
    Controls {
        transport: true,
        seek: duration_ms > 0,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Seek {
    To(u32),
    Next,
}

/// Where a seek of `offset_us` from `position_ms` lands. Before the start is
/// the start; past the end is the next track, as MPRIS asks.
fn seek_by(position_ms: u32, offset_us: i64, duration_ms: u32) -> Seek {
    let target = i64::from(position_ms).saturating_add(offset_us / 1000);
    if target > i64::from(duration_ms) {
        Seek::Next
    } else {
        Seek::To(target.max(0) as u32)
    }
}

/// Where a jump to `position_us` lands: only on the track it names, and only
/// inside it. Anything else is ignored, as MPRIS asks.
fn set_position(
    current: Option<&str>,
    id: &str,
    position_us: i64,
    duration_ms: u32,
) -> Option<u32> {
    if current != Some(id) || position_us < 0 {
        return None;
    }
    let ms = position_us / 1000;
    (ms <= i64::from(duration_ms)).then_some(ms as u32)
}

/// A client's volume as a percentage: beyond full is full, below silent is
/// silent.
fn volume_percent(volume: f64) -> Option<f64> {
    volume.is_finite().then(|| volume.clamp(0.0, 1.0) * 100.0)
}

fn loop_status(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Off => "None",
        Repeat::Context => "Playlist",
        Repeat::Track => "Track",
    }
}

fn repeat_from(loop_status: &str) -> Option<Repeat> {
    match loop_status {
        "None" => Some(Repeat::Off),
        "Playlist" => Some(Repeat::Context),
        "Track" => Some(Repeat::Track),
        _ => None,
    }
}

/// Whether a place the engine names is a jump: on the same track, away from
/// where playback was expected to be by now; on another track, anywhere but
/// its start. Reports drift a little from the expected place, so a small
/// difference is playback running on, not a seek.
fn jumped(expected: Option<(&str, u32)>, track: &str, position_ms: u32) -> bool {
    const SLACK_MS: u32 = 1_500;
    match expected {
        Some((was, at)) if was == track => position_ms.abs_diff(at) > SLACK_MS,
        _ => position_ms > SLACK_MS,
    }
}

/// The playing track as MPRIS metadata (`a{sv}`).
fn metadata(info: &TrackInfo) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    if let Some(path) = track_id(&info.uri)
        && let Ok(path) = glib::variant::ObjectPath::try_from(path)
    {
        dict.insert_value("mpris:trackid", &path.to_variant());
    }
    dict.insert_value("xesam:title", &info.title.to_variant());
    dict.insert_value("xesam:artist", &info.artists.to_variant());
    dict.insert_value("xesam:album", &info.album.to_variant());
    dict.insert_value(
        "mpris:length",
        &(i64::from(info.duration_ms) * 1000).to_variant(),
    );
    if let Some(cover) = info.cover_for(300) {
        dict.insert_value("mpris:artUrl", &cover.url().to_variant());
    }
    dict.end()
}

/// What the shell should show for a supervisor status. Waiting states count
/// as paused: nothing plays, but playback has not been given up.
fn playback_status(status: &Status) -> &'static str {
    match status {
        Status::Playing { .. } | Status::Loading { .. } => "Playing",
        Status::Paused { .. }
        | Status::Throttled { .. }
        | Status::Reconnecting { .. }
        | Status::OutputFailed { .. } => "Paused",
        Status::Idle | Status::Blocked => "Stopped",
    }
}

/// An MPRIS track id is a D-Bus object path; Spotify ids are base62, so they
/// fit. Tracks and episodes (played here over Connect) get one; local files,
/// whose uris are not ids, do not.
fn track_id(uri: &str) -> Option<String> {
    let (kind, id) = uri.strip_prefix("spotify:")?.split_once(':')?;
    if !matches!(kind, "track" | "episode") {
        return None;
    }
    let path = format!("/io/github/develonrails/Descant/{kind}/{id}");
    glib::Variant::is_object_path(&path).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACK: &str = "spotify:track:1111111111111111111111";

    fn paused() -> Status {
        Status::Paused {
            track: TRACK.into(),
        }
    }

    #[test]
    fn waiting_is_paused_not_stopped() {
        let t = TRACK.to_string();
        assert_eq!(
            playback_status(&Status::Playing { track: t.clone() }),
            "Playing"
        );
        assert_eq!(
            playback_status(&Status::Throttled {
                track: t,
                retry_at: Default::default()
            }),
            "Paused"
        );
        assert_eq!(playback_status(&Status::Idle), "Stopped");
    }

    #[test]
    fn spotify_ids_make_valid_track_paths() {
        let track = track_id("spotify:track:3n3Ppam7vgaVa1iaRUc9Lp").unwrap();
        let episode = track_id("spotify:episode:3n3Ppam7vgaVa1iaRUc9Lp").unwrap();
        assert_ne!(track, episode, "the same id names two different things");
        assert!(track_id("spotify:show:3n3Ppam7vgaVa1iaRUc9Lp").is_none());
        assert!(track_id("spotify:local:a:b:c:180").is_none());
    }

    #[test]
    fn metadata_carries_what_the_shell_shows() {
        let info = TrackInfo {
            uri: "spotify:track:3n3Ppam7vgaVa1iaRUc9Lp".into(),
            title: "Song".into(),
            artists: vec!["A".into(), "B".into()],
            album: "Album".into(),
            duration_ms: 200_000,
            ..Default::default()
        };
        let m = metadata(&info);
        assert_eq!(m.type_().as_str(), "a{sv}");
        let dict = glib::VariantDict::new(Some(&m));
        assert_eq!(
            dict.lookup::<String>("xesam:title").unwrap().as_deref(),
            Some("Song")
        );
        assert_eq!(
            dict.lookup::<Vec<String>>("xesam:artist").unwrap(),
            Some(vec!["A".to_string(), "B".to_string()])
        );
        assert_eq!(
            dict.lookup::<i64>("mpris:length").unwrap(),
            Some(200_000_000)
        );
        assert_eq!(
            dict.lookup_value("mpris:trackid", None)
                .map(|v| v.type_().as_str().to_string()),
            Some("o".to_string())
        );
    }

    #[test]
    fn the_declared_interfaces_parse() {
        let node = gio::DBusNodeInfo::for_xml(INTERFACES).unwrap();
        assert_eq!(node.interfaces().len(), 2);
    }

    #[test]
    fn nothing_to_control_without_a_track() {
        assert_eq!(controls(&Status::Idle, false, 0), Controls::default());
        assert_eq!(
            controls(&paused(), true, 200_000),
            Controls {
                transport: true,
                seek: true
            }
        );
    }

    #[test]
    fn the_last_run_s_track_can_be_played_before_anything_loads() {
        assert!(controls(&Status::Idle, true, 200_000).transport);
    }

    #[test]
    fn an_account_that_cannot_play_offers_nothing() {
        assert_eq!(
            controls(&Status::Blocked, true, 200_000),
            Controls::default()
        );
    }

    #[test]
    fn a_track_of_unknown_length_cannot_be_sought_in() {
        let c = controls(&paused(), true, 0);
        assert!(c.transport);
        assert!(!c.seek);
    }

    #[test]
    fn a_relative_seek_stays_inside_the_track_or_moves_on() {
        assert_eq!(seek_by(10_000, 5_000_000, 200_000), Seek::To(15_000));
        assert_eq!(seek_by(10_000, -20_000_000, 200_000), Seek::To(0));
        assert_eq!(seek_by(190_000, 15_000_000, 200_000), Seek::Next);
        assert_eq!(seek_by(10_000, i64::MAX, 200_000), Seek::Next);
        assert_eq!(seek_by(10_000, i64::MIN, 200_000), Seek::To(0));
    }

    #[test]
    fn a_position_is_only_set_on_the_track_it_names() {
        let id = "/io/github/develonrails/Descant/track/a";
        assert_eq!(
            set_position(Some(id), id, 30_000_000, 200_000),
            Some(30_000)
        );
        assert_eq!(set_position(Some(id), "/other", 30_000_000, 200_000), None);
        assert_eq!(set_position(None, id, 30_000_000, 200_000), None);
        assert_eq!(set_position(Some(id), id, -1, 200_000), None);
        assert_eq!(set_position(Some(id), id, 200_001_000, 200_000), None);
        assert_eq!(
            set_position(Some(id), id, 200_000_000, 200_000),
            Some(200_000)
        );
    }

    #[test]
    fn a_client_s_volume_is_kept_in_range() {
        assert_eq!(volume_percent(0.5), Some(50.0));
        assert_eq!(volume_percent(1.7), Some(100.0));
        assert_eq!(volume_percent(-0.2), Some(0.0));
        assert_eq!(volume_percent(f64::NAN), None);
    }

    #[test]
    fn repeat_modes_are_loop_statuses_and_back() {
        for mode in [Repeat::Off, Repeat::Context, Repeat::Track] {
            assert_eq!(repeat_from(loop_status(mode)), Some(mode));
        }
        assert_eq!(loop_status(Repeat::Off), "None");
        assert_eq!(repeat_from("Shuffle"), None);
    }

    #[test]
    fn running_on_is_not_a_jump() {
        assert!(!jumped(Some((TRACK, 10_000)), TRACK, 10_400));
        assert!(!jumped(Some(("spotify:track:x", 180_000)), TRACK, 0));
        assert!(!jumped(None, TRACK, 0));
    }

    #[test]
    fn a_seek_or_a_mid_track_start_is_a_jump() {
        assert!(jumped(Some((TRACK, 10_000)), TRACK, 60_000));
        assert!(jumped(Some((TRACK, 60_000)), TRACK, 10_000));
        assert!(jumped(None, TRACK, 90_000));
    }

    #[test]
    fn a_new_status_announces_what_it_changes_and_nothing_else() {
        let mut state = State::new();
        state.track = Some(Shown {
            id: track_id(TRACK),
            duration_ms: 200_000,
        });
        state.status = Status::Blocked;
        let before = signalled(&state);
        state.status = paused();
        let names: Vec<_> = changes(&before, &signalled(&state))
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            names,
            [
                "PlaybackStatus",
                "CanGoNext",
                "CanGoPrevious",
                "CanPlay",
                "CanPause",
                "CanSeek"
            ]
        );
    }

    /// The real thing, on a private bus: a client reads, writes and calls the
    /// way the shell and GSConnect do, and that reaches Descant's actions.
    #[test]
    fn a_client_on_the_bus_reaches_descant() {
        use std::{
            thread,
            time::{Duration, Instant},
        };

        let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
        bus.up();
        let address = bus.bus_address().unwrap().to_string();
        let context = glib::MainContext::new();
        let (read, done) = context
            .with_thread_default(|| {
                let app = gio::Application::new(
                    Some("io.github.develonrails.Descant.Test"),
                    gio::ApplicationFlags::empty(),
                );
                app.register(None::<&gio::Cancellable>).unwrap();
                let done = Rc::new(RefCell::new(Vec::<String>::new()));
                let note = |what: String| {
                    let done = done.clone();
                    move || done.borrow_mut().push(what.clone())
                };
                let (seek, volume, shuffle, repeat) =
                    (done.clone(), done.clone(), done.clone(), done.clone());
                let actions = Actions {
                    play_pause: Box::new(note("play-pause".into())),
                    play: Box::new(note("play".into())),
                    pause: Box::new(note("pause".into())),
                    next: Box::new(note("next".into())),
                    previous: Box::new(note("previous".into())),
                    seek: Box::new(move |ms| seek.borrow_mut().push(format!("seek {ms}"))),
                    volume: Box::new(move |p| volume.borrow_mut().push(format!("volume {p}"))),
                    shuffle: Box::new(move |on| shuffle.borrow_mut().push(format!("shuffle {on}"))),
                    repeat: Box::new(move |m| repeat.borrow_mut().push(format!("repeat {m:?}"))),
                    raise: Box::new(note("raise".into())),
                    quit: Box::new(note("quit".into())),
                    position: Box::new(|| 42_000),
                };
                let mpris = Mpris::start(&app, actions).unwrap();
                mpris.track(&TrackInfo {
                    uri: TRACK.into(),
                    duration_ms: 200_000,
                    ..Default::default()
                });
                mpris.status(&paused());

                let client = thread::spawn(move || {
                    let bus = gio::DBusConnection::for_address_sync(
                        &address,
                        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                        None,
                        None::<&gio::Cancellable>,
                    )
                    .unwrap();
                    let call = |interface: &str, method: &str, args: Option<glib::Variant>| {
                        bus.call_sync(
                            Some(BUS_NAME),
                            PATH,
                            interface,
                            method,
                            args.as_ref(),
                            None,
                            gio::DBusCallFlags::NONE,
                            2_000,
                            None::<&gio::Cancellable>,
                        )
                    };
                    let properties = "org.freedesktop.DBus.Properties";
                    let get = |name: &str| {
                        call(properties, "Get", Some((PLAYER, name).to_variant()))
                            .map(|reply| reply.child_value(0).as_variant().unwrap())
                    };
                    let set = |name: &str, value: glib::Variant| {
                        call(properties, "Set", Some((PLAYER, name, value).to_variant()))
                    };
                    // The name is Descant's once its side has run its loop a little.
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while get("CanSeek").is_err() {
                        assert!(Instant::now() < deadline, "the bus name never appeared");
                        thread::sleep(Duration::from_millis(20));
                    }
                    let read = ["CanSeek", "Position", "LoopStatus", "PlaybackStatus"]
                        .map(|name| get(name).unwrap());

                    set("Volume", 0.25.to_variant()).unwrap();
                    set("Shuffle", true.to_variant()).unwrap();
                    set("LoopStatus", "Track".to_variant()).unwrap();
                    assert!(set("LoopStatus", "Sideways".to_variant()).is_err());
                    assert!(set("CanSeek", false.to_variant()).is_err(), "read-only");

                    call(PLAYER, "Seek", Some((5_000_000i64,).to_variant())).unwrap();
                    let path = |p: &str| glib::variant::ObjectPath::try_from(p).unwrap();
                    let this = path(&track_id(TRACK).unwrap());
                    let other = path("/io/github/develonrails/Descant/track/other");
                    let at = |id, us: i64| Some((id, us).to_variant());
                    call(PLAYER, "SetPosition", at(this, 10_000_000)).unwrap();
                    call(PLAYER, "SetPosition", at(other, 20_000_000)).unwrap();
                    call(PLAYER, "PlayPause", None).unwrap();
                    read
                });
                // Descant's side: serve until the client is done.
                while !client.is_finished() {
                    context.iteration(false);
                    thread::sleep(Duration::from_millis(1));
                }
                let read = client.join().unwrap();
                // The bus goes away next; without this, losing it ends the process.
                app.dbus_connection().unwrap().set_exit_on_close(false);
                drop((mpris, app));
                while context.iteration(false) {}
                (read, done.take())
            })
            .unwrap();
        bus.down();

        assert_eq!(read[0].get::<bool>(), Some(true));
        assert_eq!(read[1].get::<i64>(), Some(42_000_000));
        assert_eq!(read[2].str(), Some("None"));
        assert_eq!(read[3].str(), Some("Paused"));
        assert_eq!(
            done,
            [
                "volume 25",
                "shuffle true",
                "repeat Track",
                "seek 47000",
                "seek 10000",
                "play-pause"
            ]
        );
    }

    #[test]
    fn the_position_never_passes_the_end_of_the_track() {
        let mut state = State::new();
        assert_eq!(property("Position", &state, 5_000).get::<i64>(), Some(0));
        state.track = Some(Shown {
            id: None,
            duration_ms: 200_000,
        });
        assert_eq!(
            property("Position", &state, 5_000).get::<i64>(),
            Some(5_000_000)
        );
        assert_eq!(
            property("Position", &state, 250_000).get::<i64>(),
            Some(200_000_000)
        );
    }
}
