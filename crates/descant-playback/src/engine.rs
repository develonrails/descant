//! The librespot engine: a Player driven through Spirc.
//!
//! Going through Spirc makes Descant a Spotify Connect device for free, and
//! leaves context, queue, shuffle and autoplay to librespot-connect. The
//! engine applies the supervisor's effects and reports everything the player
//! does as [`Report`]s.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use descant_core::{
    quality::Quality,
    supervisor::{Command, Effect, Input},
    track::{Cover, TrackInfo},
};
use librespot_connect::{ConnectConfig, LoadRequest, LoadRequestOptions, PlayingTrack, Spirc};
use librespot_core::{Error, authentication::Credentials, session::Session};
use librespot_metadata::audio::{AudioItem, UniqueFields};
use librespot_playback::{
    config::{Bitrate, PlayerConfig},
    mixer::{self, MixerConfig},
    player::{Player, PlayerEvent},
};
use tokio::{sync::mpsc::UnboundedSender, task::JoinHandle};

use crate::{
    equalizer::{EqualizedSink, EqualizerSetting},
    pulse::PulseSink,
    sink::GuardedSink,
};

/// What the engine tells its host.
#[derive(Debug, Clone)]
pub enum Report {
    /// Feed this to the supervisor.
    Input(Input),
    /// The track that now plays, for the UI.
    NowPlaying(TrackInfo),
    /// The tracks coming up after the current one, in order.
    Queue(Vec<String>),
    /// The volume changed (here or from another Connect device), 0–65535.
    Volume(u16),
}

/// The context as Spirc last reported it, and the track that loads now.
///
/// Spirc reports the queue (`SetQueue`) when a context is loaded or the queue
/// changes, not on every track change, so the order is kept whole and the
/// current track is followed from the player's own `Loading` events.
#[derive(Default, Debug)]
struct Queue {
    context: Option<String>,
    /// The context's tracks in play order: previous, current, next.
    tracks: Vec<String>,
    current: Option<String>,
}

/// The context Spirc reports for a plain list of tracks (top songs, a
/// single track); Spotify cannot load it again by that name.
const TRACK_LIST: &str = "spotify:web-api";

/// Whether `context` can be loaded again by its URI.
pub fn reloadable(context: &str) -> bool {
    context.starts_with("spotify:") && context != TRACK_LIST
}

/// What to load to play a track again within what was playing.
#[derive(Debug, PartialEq, Eq)]
enum Again {
    Context(String),
    Tracks(Vec<String>),
}

impl Again {
    fn request(self, options: LoadRequestOptions) -> LoadRequest {
        match self {
            Again::Context(context) => LoadRequest::from_context_uri(context, options),
            Again::Tracks(tracks) => LoadRequest::from_tracks(tracks, options),
        }
    }
}

impl Queue {
    /// How to play `track` again: in its context, in its track list, or on
    /// its own.
    fn again(&self, track: &str) -> Again {
        match &self.context {
            Some(context) if reloadable(context) => Again::Context(context.clone()),
            _ if self.tracks.iter().any(|t| t == track) => Again::Tracks(self.tracks.clone()),
            _ => Again::Tracks(vec![track.to_string()]),
        }
    }

    /// What comes after the current track (at most a hundred).
    fn upcoming(&self) -> Vec<String> {
        let start = self
            .current
            .as_ref()
            .and_then(|c| self.tracks.iter().position(|t| t == c))
            .map_or(0, |i| i + 1);
        self.tracks.iter().skip(start).take(100).cloned().collect()
    }

    /// The track `n` places after the current one, if the queue knows it.
    fn target(&self, n: usize) -> Option<String> {
        let current = self.current.as_ref()?;
        let index = self.tracks.iter().position(|t| t == current)?;
        self.tracks.get(index + n).cloned()
    }
}

pub struct LibrespotEngine {
    session: Session,
    spirc: Spirc,
    player: Arc<Player>,
    queue: Arc<Mutex<Queue>>,
    closing: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
}

/// What librespot calls the stream Spotify sends.
fn bitrate(quality: Quality) -> Bitrate {
    match quality {
        Quality::Normal => Bitrate::Bitrate96,
        Quality::High => Bitrate::Bitrate160,
        Quality::VeryHigh => Bitrate::Bitrate320,
    }
}

impl LibrespotEngine {
    /// Connects `session` with `credentials`, appears as a Connect device
    /// called `device_name`, and reports to `reports` until shut down.
    pub async fn start(
        session: Session,
        credentials: Credentials,
        device_name: &str,
        initial_volume: u16,
        quality: Quality,
        equalizer: EqualizerSetting,
        reports: UnboundedSender<Report>,
    ) -> Result<LibrespotEngine, Error> {
        let mixer = mixer::find(Some("softvol")).ok_or_else(|| Error::unavailable("softvol"))?(
            MixerConfig::default(),
        )?;

        let sink_reports = reports.clone();
        let player = Player::new(
            PlayerConfig {
                bitrate: bitrate(quality),
                // the sink plays floats, which need no dither
                ditherer: None,
                ..PlayerConfig::default()
            },
            session.clone(),
            mixer.get_soft_volume(),
            move || {
                let guarded = GuardedSink::new(
                    Box::new(|| Box::new(PulseSink::new(None))),
                    Box::new(move |message| {
                        let _ = sink_reports.send(Report::Input(Input::SinkFailed { message }));
                    }),
                );
                Box::new(EqualizedSink::new(Box::new(guarded), equalizer))
            },
        );
        let mut events = player.get_player_event_channel();

        let config = ConnectConfig {
            name: device_name.to_string(),
            initial_volume,
            emit_set_queue_events: true,
            ..Default::default()
        };
        let (spirc, spirc_task) =
            Spirc::new(config, session.clone(), credentials, player.clone(), mixer).await?;

        let closing = Arc::new(AtomicBool::new(false));
        let queue = Arc::new(Mutex::new(Queue::default()));

        // Spirc's task ends when the session or the dealer connection dies.
        let lost = reports.clone();
        let is_closing = closing.clone();
        let spirc_handle = tokio::spawn(async move {
            spirc_task.await;
            if !is_closing.load(Ordering::SeqCst) {
                let _ = lost.send(Report::Input(Input::SessionLost));
            }
        });

        let event_queue = queue.clone();
        let events_handle = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                for report in report_for(event, &event_queue) {
                    if reports.send(report).is_err() {
                        return;
                    }
                }
            }
        });

        Ok(LibrespotEngine {
            session,
            spirc,
            player,
            queue,
            closing,
            tasks: vec![spirc_handle, events_handle],
        })
    }

    /// The connected session, for reading the library through it.
    pub fn session(&self) -> Session {
        self.session.clone()
    }

    /// The context playing now, as Spirc reported it.
    pub fn context(&self) -> Option<String> {
        self.queue.lock().unwrap().context.clone()
    }

    /// Where the next reload plays: the context of an earlier engine (after a
    /// reconnect) or of an earlier run (after a restart).
    pub fn set_context(&self, context: Option<String>) {
        self.queue.lock().unwrap().context = context;
    }

    /// Carries out one of the supervisor's effects. `Reconnect` and `Notify`
    /// are the host's business and ignored here.
    pub fn apply(&self, effect: &Effect) -> Result<(), Error> {
        match effect {
            Effect::Send(command) => self.send(command),
            Effect::StopPlayer => {
                // Stop now rather than let librespot play the track undecrypted.
                self.spirc.pause()?;
                self.player.stop();
                Ok(())
            }
            Effect::Reload { track, position_ms } => {
                let again = self.queue.lock().unwrap().again(track);
                let options = LoadRequestOptions {
                    start_playing: true,
                    seek_to: *position_ms,
                    playing_track: Some(PlayingTrack::Uri(track.clone())),
                    ..Default::default()
                };
                self.spirc.activate()?;
                self.spirc.load(again.request(options))
            }
            Effect::Reconnect | Effect::Notify(_) => Ok(()),
        }
    }

    fn send(&self, command: &Command) -> Result<(), Error> {
        match command {
            // The controller answers this by building a new player, so it
            // never reaches an engine.
            Command::SetQuality(_) => Ok(()),
            Command::Play => self.spirc.play(),
            Command::Pause => self.spirc.pause(),
            Command::Seek(ms) => self.spirc.set_position_ms(*ms),
            Command::Skip(n) => self.skip(*n),
            Command::PlayTracks {
                tracks,
                start,
                position_ms,
            } => {
                let options = LoadRequestOptions {
                    start_playing: true,
                    seek_to: *position_ms,
                    playing_track: Some(PlayingTrack::Index(*start as u32)),
                    ..Default::default()
                };
                self.spirc.activate()?;
                self.spirc
                    .load(LoadRequest::from_tracks(tracks.clone(), options))
            }
            Command::Shuffle(on) => self.spirc.shuffle(*on),
            Command::SetVolume(volume) => self.spirc.set_volume(*volume),
            Command::Repeat(mode) => {
                use descant_core::supervisor::Repeat;
                self.spirc.repeat(*mode == Repeat::Context)?;
                self.spirc.repeat_track(*mode == Repeat::Track)
            }
            Command::AddToQueue(uri) => {
                let uri = librespot_core::SpotifyUri::from_uri(uri)
                    .map_err(|_| Error::invalid_argument("not a Spotify URI"))?;
                self.spirc.add_to_queue(uri)
            }
            Command::Load {
                context,
                start_track,
            } => {
                let options = LoadRequestOptions {
                    start_playing: true,
                    playing_track: start_track.clone().map(PlayingTrack::Uri),
                    ..Default::default()
                };
                // A single track or episode is a list of one; anything else
                // is a context Spotify knows by its URI.
                let request = if context.starts_with("spotify:track:")
                    || context.starts_with("spotify:episode:")
                {
                    LoadRequest::from_tracks(vec![context.clone()], options)
                } else {
                    LoadRequest::from_context_uri(context.clone(), options)
                };
                self.spirc.activate()?;
                self.spirc.load(request)
            }
        }
    }

    /// A skip of more than one track is a single load of the target, so it
    /// costs one audio key instead of one per track passed.
    fn skip(&self, n: i32) -> Result<(), Error> {
        match n {
            0 => Ok(()),
            1 => self.spirc.next(),
            -1 => self.spirc.prev(),
            n if n > 1 => {
                let target = {
                    let queue = self.queue.lock().unwrap();
                    queue.target(n as usize).map(|t| (queue.again(&t), t))
                };
                match target {
                    Some((again, track)) => {
                        let options = LoadRequestOptions {
                            start_playing: true,
                            playing_track: Some(PlayingTrack::Uri(track)),
                            ..Default::default()
                        };
                        self.spirc.load(again.request(options))
                    }
                    None => (0..n).try_for_each(|_| self.spirc.next()),
                }
            }
            n => (0..-n).try_for_each(|_| self.spirc.prev()),
        }
    }

    /// Stops reporting and leaves Spotify Connect.
    pub fn shutdown(self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.spirc.shutdown();
        for task in self.tasks {
            task.abort();
        }
    }
}

fn report_for(event: PlayerEvent, queue: &Mutex<Queue>) -> Vec<Report> {
    let input = match event {
        PlayerEvent::Loading { track_id, .. } => {
            let track = track_id.to_uri();
            let mut queue = queue.lock().unwrap();
            queue.current = Some(track.clone());
            return vec![
                Report::Input(Input::Loading { track }),
                Report::Queue(queue.upcoming()),
            ];
        }
        PlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        } => Input::Playing {
            track: track_id.to_uri(),
            position_ms,
        },
        PlayerEvent::Paused {
            track_id,
            position_ms,
            ..
        } => Input::Paused {
            track: track_id.to_uri(),
            position_ms,
        },
        PlayerEvent::Seeked {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::PositionCorrection {
            track_id,
            position_ms,
            ..
        } => Input::Seeked {
            track: track_id.to_uri(),
            position_ms,
        },
        PlayerEvent::Stopped { .. } => Input::Stopped,
        PlayerEvent::Unavailable { track_id, .. } => Input::Unavailable {
            track: track_id.to_uri(),
        },
        PlayerEvent::VolumeChanged { volume } => return vec![Report::Volume(volume)],
        PlayerEvent::TrackChanged { audio_item } => {
            return vec![Report::NowPlaying(track_info(&audio_item))];
        }
        PlayerEvent::SetQueue {
            context_uri,
            current_track,
            next_tracks,
            prev_tracks,
        } => {
            let mut queue = queue.lock().unwrap();
            queue.context = Some(context_uri);
            queue.tracks = prev_tracks
                .into_iter()
                .chain(current_track)
                .chain(next_tracks)
                .map(|t| t.uri)
                .collect();
            return vec![Report::Queue(queue.upcoming())];
        }
        _ => return vec![],
    };
    vec![Report::Input(input)]
}

/// The player's covers often carry no width; their size class says enough.
fn cover_width(cover: &librespot_metadata::audio::item::CoverImage) -> u32 {
    use librespot_metadata::image::ImageSize;
    if cover.width > 0 {
        return cover.width as u32;
    }
    match cover.size {
        ImageSize::SMALL => 64,
        ImageSize::DEFAULT => 300,
        ImageSize::LARGE => 640,
        ImageSize::XLARGE => 1280,
    }
}

fn track_info(item: &AudioItem) -> TrackInfo {
    let artist_uris = match &item.unique_fields {
        UniqueFields::Track { artists, .. } => artists.iter().map(|a| a.id.to_uri()).collect(),
        _ => vec![],
    };
    let (artists, album) = match &item.unique_fields {
        UniqueFields::Track { artists, album, .. } => (
            artists.iter().map(|a| a.name.clone()).collect(),
            album.clone(),
        ),
        UniqueFields::Local { artists, album, .. } => (
            artists.iter().cloned().collect(),
            album.clone().unwrap_or_default(),
        ),
        // Sent here over Connect: the show stands where the artists would,
        // in the bar and in the media controls.
        UniqueFields::Episode { show_name, .. } => (vec![show_name.clone()], String::new()),
    };
    let mut covers: Vec<Cover> = item
        .covers
        .iter()
        .map(|c| Cover {
            url: c.url.clone(),
            width: cover_width(c),
        })
        .collect();
    covers.sort_by_key(|c| c.width);
    TrackInfo {
        uri: item.uri.clone(),
        title: item.name.clone(),
        artists,
        artist_uris,
        album,
        // AudioItem names the album but does not identify it.
        album_uri: String::new(),
        duration_ms: item.duration_ms,
        covers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_core::{SpotifyId, SpotifyUri};
    use librespot_playback::player::QueueTrack;

    fn qt(s: &str) -> QueueTrack {
        QueueTrack {
            uri: s.to_string(),
            provider: "context".into(),
        }
    }

    fn episode(show: &str) -> AudioItem {
        let id = "3n3Ppam7vgaVa1iaRUc9Lp";
        AudioItem {
            track_id: SpotifyUri::Episode {
                id: SpotifyId::from_base62(id).unwrap(),
            },
            uri: format!("spotify:episode:{id}"),
            files: Default::default(),
            name: "Episode 12".into(),
            covers: vec![],
            language: vec![],
            duration_ms: 1_800_000,
            is_explicit: false,
            availability: Ok(()),
            alternatives: None,
            unique_fields: UniqueFields::Episode {
                description: String::new(),
                publish_time: librespot_core::date::Date::now_utc(),
                show_name: show.into(),
            },
        }
    }

    #[test]
    fn an_episode_names_its_show_where_a_track_names_its_artists() {
        let info = track_info(&episode("The Show"));
        assert_eq!(info.title, "Episode 12");
        assert_eq!(info.artists, ["The Show"]);
        assert_eq!(info.artist_line(), "The Show");
        assert!(info.artist_uris.is_empty(), "a show is no artist to open");
    }

    fn loading(s: &str) -> PlayerEvent {
        let id = s.strip_prefix("spotify:track:").unwrap();
        PlayerEvent::Loading {
            play_request_id: 1,
            track_id: SpotifyUri::Track {
                id: SpotifyId::from_base62(id).unwrap(),
            },
            position_ms: 0,
        }
    }

    const A: &str = "spotify:track:1111111111111111111111";
    const B: &str = "spotify:track:2222222222222222222222";
    const C: &str = "spotify:track:3333333333333333333333";
    const D: &str = "spotify:track:4444444444444444444444";
    const E: &str = "spotify:track:5555555555555555555555";

    fn queue_after_load() -> Mutex<Queue> {
        let q = Mutex::new(Queue::default());
        report_for(
            PlayerEvent::SetQueue {
                context_uri: "spotify:playlist:x".into(),
                current_track: Some(qt(A)),
                next_tracks: vec![qt(B), qt(C), qt(D), qt(E)],
                prev_tracks: vec![],
            },
            &q,
        );
        q
    }

    #[test]
    fn skip_targets_follow_the_track_that_loads_not_the_stale_queue() {
        let q = queue_after_load();
        report_for(loading(A), &q);
        assert_eq!(q.lock().unwrap().target(2).as_deref(), Some(C));
        // Spirc moved on to B without a new SetQueue:
        report_for(loading(B), &q);
        assert_eq!(q.lock().unwrap().target(2).as_deref(), Some(D));
        assert_eq!(q.lock().unwrap().target(3).as_deref(), Some(E));
    }

    #[test]
    fn the_queue_report_is_what_comes_after_the_track_that_loads() {
        let q = queue_after_load();
        let reports = report_for(loading(B), &q);
        let upcoming = reports.iter().find_map(|r| match r {
            Report::Queue(list) => Some(list.clone()),
            _ => None,
        });
        assert_eq!(
            upcoming,
            Some(vec![C.to_string(), D.to_string(), E.to_string()])
        );
    }

    #[test]
    fn no_target_past_the_end_or_for_an_unknown_track() {
        let q = queue_after_load();
        report_for(loading(D), &q);
        assert_eq!(q.lock().unwrap().target(2), None);
        let fresh = Mutex::new(Queue::default());
        report_for(loading(A), &fresh);
        assert_eq!(fresh.lock().unwrap().target(2), None);
    }

    #[test]
    fn a_track_plays_again_in_its_context() {
        let q = queue_after_load();
        assert_eq!(
            q.lock().unwrap().again(C),
            Again::Context("spotify:playlist:x".into())
        );
    }

    #[test]
    fn a_plain_track_list_plays_again_as_that_list() {
        let q = Mutex::new(Queue::default());
        report_for(
            PlayerEvent::SetQueue {
                context_uri: TRACK_LIST.into(),
                current_track: Some(qt(A)),
                next_tracks: vec![qt(B)],
                prev_tracks: vec![],
            },
            &q,
        );
        let q = q.lock().unwrap();
        assert_eq!(q.again(B), Again::Tracks(vec![A.into(), B.into()]));
        // a track from somewhere else plays on its own
        assert_eq!(q.again(C), Again::Tracks(vec![C.into()]));
    }

    #[test]
    fn only_real_contexts_can_be_loaded_again() {
        assert!(reloadable("spotify:playlist:x"));
        assert!(reloadable("spotify:user:me:collection"));
        assert!(!reloadable(TRACK_LIST));
        assert!(!reloadable(""));
    }

    #[test]
    fn a_seek_is_reported() {
        let event = PlayerEvent::Seeked {
            play_request_id: 1,
            track_id: SpotifyUri::Track {
                id: SpotifyId::from_base62(&A[14..]).unwrap(),
            },
            position_ms: 90_000,
        };
        let reports = report_for(event, &Mutex::new(Queue::default()));
        assert!(matches!(
            reports.as_slice(),
            [Report::Input(Input::Seeked {
                position_ms: 90_000,
                ..
            })]
        ));
    }
}
