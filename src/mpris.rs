//! Media keys and GNOME's media controls, over MPRIS.
//!
//! Descant owns `org.mpris.MediaPlayer2.Descant` on the session bus: the shell's
//! media controls, keyboard media keys and headset buttons all talk to it.
//! It is served on the D-Bus connection GApplication already has, through
//! GDBus: no second connection, and no runtime or thread of its own.

use std::{cell::RefCell, rc::Rc};

use descant_core::{supervisor::Status, track::TrackInfo};
use gtk::{gio, glib, prelude::*};

const BUS_NAME: &str = "org.mpris.MediaPlayer2.Descant";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// The two interfaces, as much of them as Descant does. Seeking and opening
/// URIs are declared (the spec asks for them) but not offered.
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
    <property name="Rate" type="d" access="read"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="read"/>
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

/// What the MPRIS buttons do.
pub struct Actions {
    pub play_pause: Box<dyn Fn()>,
    pub play: Box<dyn Fn()>,
    pub pause: Box<dyn Fn()>,
    pub next: Box<dyn Fn()>,
    pub previous: Box<dyn Fn()>,
    pub raise: Box<dyn Fn()>,
    pub quit: Box<dyn Fn()>,
}

struct State {
    status: &'static str,
    metadata: glib::Variant,
}

pub struct Mpris {
    connection: gio::DBusConnection,
    state: Rc<RefCell<State>>,
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
        let state = Rc::new(RefCell::new(State {
            status: "Stopped",
            metadata: glib::VariantDict::new(None).end(),
        }));
        let actions = Rc::new(actions);
        for interface in node.interfaces() {
            let (actions, state) = (actions.clone(), state.clone());
            let registered = connection
                .register_object(PATH, interface)
                .method_call(move |_, _, _, _, method, _, invocation| {
                    match method {
                        "Raise" => (actions.raise)(),
                        "Quit" => (actions.quit)(),
                        "Next" => (actions.next)(),
                        "Previous" => (actions.previous)(),
                        "Pause" | "Stop" => (actions.pause)(),
                        "PlayPause" => (actions.play_pause)(),
                        "Play" => (actions.play)(),
                        _ => {} // Seek, SetPosition, OpenUri: not offered
                    }
                    invocation.return_value(None);
                })
                .property(move |_, _, _, _, name| property(name, &state.borrow()))
                .build();
            if let Err(e) = registered {
                log::warn!("MPRIS unavailable: {e}");
                return None;
            }
        }
        gio::bus_own_name_on_connection(
            &connection,
            BUS_NAME,
            gio::BusNameOwnerFlags::NONE,
            |_, _| {},
            |_, name| log::warn!("could not own {name}; media keys will not reach Descant"),
        );
        Some(Mpris { connection, state })
    }

    pub fn status(&self, status: &Status) {
        let status = playback_status(status);
        if std::mem::replace(&mut self.state.borrow_mut().status, status) != status {
            self.changed("PlaybackStatus", status.to_variant());
        }
    }

    pub fn track(&self, info: &TrackInfo) {
        let metadata = metadata(info);
        self.state.borrow_mut().metadata = metadata.clone();
        self.changed("Metadata", metadata);
    }

    /// Tells listeners a Player property changed, with its new value.
    fn changed(&self, name: &str, value: glib::Variant) {
        let dict = glib::VariantDict::new(None);
        dict.insert_value(name, &value);
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

fn property(name: &str, state: &State) -> glib::Variant {
    match name {
        "Identity" => "Descant".to_variant(),
        "DesktopEntry" => crate::APP_ID.to_variant(),
        "SupportedUriSchemes" | "SupportedMimeTypes" => Vec::<String>::new().to_variant(),
        "PlaybackStatus" => state.status.to_variant(),
        "Metadata" => state.metadata.clone(),
        "Rate" | "Volume" | "MinimumRate" | "MaximumRate" => 1.0f64.to_variant(),
        "Position" => 0i64.to_variant(),
        "HasTrackList" | "CanSeek" => false.to_variant(),
        // CanQuit, CanRaise, CanGoNext, CanGoPrevious, CanPlay, CanPause, CanControl
        _ => true.to_variant(),
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
        dict.insert_value("mpris:artUrl", &cover.url.to_variant());
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

/// An MPRIS track id is a D-Bus object path; Spotify ids are base62, so they fit.
fn track_id(uri: &str) -> Option<String> {
    let id = uri.strip_prefix("spotify:track:")?;
    let path = format!("/io/github/develonrails/Descant/track/{id}");
    glib::Variant::is_object_path(&path).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_is_paused_not_stopped() {
        let t = "spotify:track:1111111111111111111111".to_string();
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
        assert!(track_id("spotify:track:3n3Ppam7vgaVa1iaRUc9Lp").is_some());
        assert!(track_id("spotify:episode:3n3Ppam7vgaVa1iaRUc9Lp").is_none());
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
}
