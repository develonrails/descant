//! Runs the supervisor and the engine together.
//!
//! One task owns the [`Supervisor`] and the current [`LibrespotEngine`]. It
//! feeds the supervisor user commands, engine reports and clock ticks, carries
//! out the resulting effects, and sends the UI [`Update`]s. When the session
//! dies it builds a new one (and a new Spirc) from the stored credentials.
//! It reports where playback stands ([`ResumePoint`]) and can start from
//! one, so a restart picks up where the last run stopped.

use std::time::Duration;

use descant_core::{
    quality::Quality,
    supervisor::{Command, Config, Effect, Input, Notice, Status, Supervisor},
    time::Millis,
    track::TrackInfo,
};
pub use librespot_core::session::Session;
use librespot_core::{authentication::Credentials, config::SessionConfig};
use tokio::{sync::mpsc, time::Instant};

use crate::{
    engine::{self, LibrespotEngine, Report},
    equalizer::EqualizerSetting,
    tap,
};

/// What the UI hears. (Not `Debug`: a librespot `Session` isn't.)
#[derive(Clone)]
pub enum Update {
    Status(Status),
    Notice(Notice),
    NowPlaying(TrackInfo),
    /// A (new) connected session: read the library through this one.
    Session(Session),
    /// The tracks coming up, in order.
    Queue(Vec<String>),
    /// The volume, 0–65535 (changed here or from another Connect device).
    Volume(u16),
    /// Where playback stands, whenever it starts, stops, moves or changes
    /// track. While `playing`, the place runs on from `point.position_ms`.
    Resume {
        point: ResumePoint,
        playing: bool,
    },
    /// Something went wrong that is worth a line in the log or a toast.
    Error(String),
}

/// A track and a place in it, to pick up from after a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumePoint {
    /// The album, playlist or collection to play it in, when Spotify can load
    /// that again; otherwise the track plays on its own.
    pub context: Option<String>,
    pub track: String,
    pub position_ms: u32,
}

#[derive(Clone)]
pub struct ControllerConfig {
    pub device_name: String,
    pub session: SessionConfig,
    pub rules: Config,
    /// The volume to start at (and to restore after a reconnect), 0–65535.
    pub initial_volume: u16,
    /// Where the last run stopped: shown paused, and played from there.
    pub resume: Option<ResumePoint>,
    /// The equalizer curve, shared with the UI; kept across reconnects.
    pub equalizer: EqualizerSetting,
    /// Which stream to ask Spotify for. Fixed when the player is built, so
    /// changing it builds a new one.
    pub quality: Quality,
}

impl Default for ControllerConfig {
    fn default() -> ControllerConfig {
        ControllerConfig {
            device_name: "Descant".to_string(),
            session: SessionConfig::default(),
            rules: Config::default(),
            initial_volume: u16::MAX,
            resume: None,
            equalizer: EqualizerSetting::default(),
            quality: Quality::default(),
        }
    }
}

/// Sends the controller what the user asks for, and whether there is a
/// network.
#[derive(Clone)]
pub struct ControllerHandle {
    commands: mpsc::UnboundedSender<Command>,
    network: mpsc::UnboundedSender<bool>,
}

impl ControllerHandle {
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Whether the system has a network (GNOME's network monitor): while it
    /// has none, the controller does not keep building sessions that cannot
    /// connect, and it tries again as soon as one is back.
    pub fn set_network(&self, available: bool) {
        let _ = self.network.send(available);
    }
}

/// Starts the controller on the current tokio runtime. `credentials` must be
/// reusable (stored) credentials: they are used again for every reconnect.
pub fn spawn(
    credentials: Credentials,
    config: ControllerConfig,
    updates: mpsc::UnboundedSender<Update>,
) -> ControllerHandle {
    let (commands, commands_rx) = mpsc::unbounded_channel();
    let (network, network_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(credentials, config, commands_rx, network_rx, updates));
    ControllerHandle { commands, network }
}

async fn connect(
    credentials: &Credentials,
    config: &ControllerConfig,
    volume: u16,
    reports: mpsc::UnboundedSender<Report>,
) -> Result<LibrespotEngine, String> {
    let session = Session::new(config.session.clone(), None);
    LibrespotEngine::start(
        session,
        credentials.clone(),
        &config.device_name,
        volume,
        config.quality,
        config.equalizer.clone(),
        reports,
    )
    .await
    .map_err(|e| e.to_string())
}

async fn run(
    credentials: Credentials,
    mut config: ControllerConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut network: mpsc::UnboundedReceiver<bool>,
    updates: mpsc::UnboundedSender<Update>,
) {
    let (reports_tx, mut reports) = mpsc::unbounded_channel::<Report>();
    let key_reports = reports_tx.clone();
    tap::on_key_refused(Box::new(move |error| {
        let _ = key_reports.send(Report::Input(Input::KeyRefused(error)));
    }));

    let start = Instant::now();
    let now = || Millis(start.elapsed().as_millis() as u64);
    let mut supervisor = Supervisor::new(config.rules.clone());
    // The context to reload in, carried across engines (reconnects) and runs.
    let mut context = None;
    if let Some(point) = &config.resume {
        supervisor.resume_at(point.track.clone(), point.position_ms);
        context = point.context.clone();
        let _ = updates.send(Update::Status(supervisor.status().clone()));
    }
    let mut last_status = supervisor.status().clone();
    let (reconnected_tx, mut reconnected) =
        mpsc::unbounded_channel::<Result<LibrespotEngine, String>>();

    // Followed from commands and reports, so a reconnect keeps the level.
    let mut volume = config.initial_volume;
    let mut engine = None;
    let mut pending = match connect(&credentials, &config, volume, reports_tx.clone()).await {
        Ok(first) => {
            let _ = updates.send(Update::Session(first.session()));
            first.set_context(context.clone());
            engine = Some(first);
            None
        }
        Err(e) => {
            let _ = updates.send(Update::Error(format!("could not connect: {e}")));
            Some(Input::SessionLost)
        }
    };

    loop {
        let input = match pending.take() {
            Some(input) => input,
            None => {
                let deadline = supervisor.next_deadline();
                let sleep = async move {
                    match deadline {
                        Some(at) => {
                            tokio::time::sleep_until(start + Duration::from_millis(at.0)).await
                        }
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    command = commands.recv() => match command {
                        // The bitrate is fixed when the player is built, so
                        // this is a reconnect: the rules already know how to
                        // put playback back where it was.
                        Some(Command::SetQuality(quality)) => {
                            config.quality = quality;
                            Input::SessionLost
                        }
                        Some(command) => {
                            if let Command::SetVolume(level) = command {
                                volume = level;
                            }
                            Input::User(command)
                        }
                        // Every handle is gone: the UI logged out or quit.
                        None => {
                            if let Some(engine) = engine.take() {
                                engine.shutdown();
                            }
                            break;
                        }
                    },
                    Some(available) = network.recv() => Input::NetworkAvailable(available),
                    Some(report) = reports.recv() => match report {
                        Report::Input(input) => input,
                        Report::NowPlaying(info) => {
                            let _ = updates.send(Update::NowPlaying(info));
                            continue;
                        }
                        Report::Queue(upcoming) => {
                            let _ = updates.send(Update::Queue(upcoming));
                            continue;
                        }
                        Report::Volume(level) => {
                            volume = level;
                            let _ = updates.send(Update::Volume(level));
                            continue;
                        }
                    },
                    Some(result) = reconnected.recv() => match result {
                        Ok(new) => {
                            let _ = updates.send(Update::Session(new.session()));
                            new.set_context(context.clone());
                            if let Some(old) = engine.replace(new) {
                                old.shutdown();
                            }
                            Input::SessionRestored
                        }
                        Err(e) => {
                            let _ = updates.send(Update::Error(format!("reconnect failed: {e}")));
                            Input::ReconnectFailed
                        }
                    },
                    _ = sleep => Input::Tick,
                    else => break,
                }
            }
        };

        log::debug!("input {input:?}");
        let moved = matches!(
            input,
            Input::Playing { .. } | Input::Paused { .. } | Input::Seeked { .. }
        );
        for effect in supervisor.handle(now(), input) {
            log::debug!("effect {effect:?}");
            match effect {
                Effect::Notify(notice) => {
                    let _ = updates.send(Update::Notice(notice));
                }
                Effect::Reconnect => {
                    if let Some(old) = engine.take() {
                        old.shutdown();
                    }
                    let (credentials, config) = (credentials.clone(), config.clone());
                    let (reports, done) = (reports_tx.clone(), reconnected_tx.clone());
                    tokio::spawn(async move {
                        let _ = done.send(connect(&credentials, &config, volume, reports).await);
                    });
                }
                other => match &engine {
                    Some(engine) => {
                        if let Err(e) = engine.apply(&other) {
                            log::warn!("could not apply {other:?}: {e}");
                        }
                    }
                    None => log::debug!("no engine for {other:?}"),
                },
            }
        }

        if let Some(engine) = &engine {
            context = engine.context();
        }
        let changed = supervisor.status() != &last_status;
        if changed {
            last_status = supervisor.status().clone();
            log::info!("status {last_status:?}");
            let _ = updates.send(Update::Status(last_status.clone()));
        }
        if (changed || moved)
            && let Some((track, position_ms)) = supervisor.position(now())
        {
            let point = ResumePoint {
                context: context.clone().filter(|c| engine::reloadable(c)),
                track,
                position_ms,
            };
            let playing = matches!(last_status, Status::Playing { .. });
            let _ = updates.send(Update::Resume { point, playing });
        }
    }
}
