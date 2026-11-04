//! The playback supervisor: Descant's playback rules as a state machine.
//!
//! The engine (librespot through Spirc) does the actual playing. The
//! supervisor sits between the user, the engine and the clock: it forwards
//! commands, reacts to what the engine reports, and decides when to stop, wait,
//! retry or reconnect. It never claims playback that the engine has not
//! reported (no fake play), it treats Spotify's audio-key budget as scarce,
//! and it turns failures into notices the UI can show.
//!
//! Drive it with [`Supervisor::handle`], passing the current time each call.
//! Call it again with [`Input::Tick`] at [`Supervisor::next_deadline`].

use crate::{key::KeyError, time::Millis};

pub type TrackUri = String;

/// What the engine can be told to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Play,
    Pause,
    /// Skip forward (positive) or back (negative) this many tracks, in one go.
    Skip(i32),
    Seek(u32),
    /// Put a track next in the queue. Costs no audio key, so it is never held back.
    AddToQueue(TrackUri),
    /// Shuffle on or off; no audio key, never held back.
    Shuffle(bool),
    /// Repeat mode; no audio key, never held back.
    Repeat(Repeat),
    /// Volume on Connect's 0–65535 scale; never held back.
    SetVolume(u16),
    /// Play a list of tracks (top songs) or episodes, starting at `start`,
    /// `position_ms` into it: where an episode was left off, else 0.
    PlayTracks {
        tracks: Vec<TrackUri>,
        start: usize,
        position_ms: u32,
    },
    /// Play a context (album, playlist, artist, or a single track).
    Load {
        context: String,
        start_track: Option<TrackUri>,
    },
}

/// What repeats when the end is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    /// The album or playlist starts over.
    Context,
    /// The same track again.
    Track,
}

impl Repeat {
    /// Off → Context → Track → Off, as a repeat button cycles.
    pub fn next(self) -> Repeat {
        match self {
            Repeat::Off => Repeat::Context,
            Repeat::Context => Repeat::Track,
            Repeat::Track => Repeat::Off,
        }
    }
}

/// Everything the supervisor reacts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Something the user asked for.
    User(Command),
    /// The engine started loading a track.
    Loading {
        track: TrackUri,
    },
    /// The engine is producing audio for a track.
    Playing {
        track: TrackUri,
        position_ms: u32,
    },
    Paused {
        track: TrackUri,
        position_ms: u32,
    },
    Stopped,
    /// The position jumped (a seek from here or another Connect device).
    Seeked {
        track: TrackUri,
        position_ms: u32,
    },
    /// The engine could not play a track (region, removed, undecodable).
    Unavailable {
        track: TrackUri,
    },
    /// Spotify refused the audio key for the track being loaded.
    KeyRefused(KeyError),
    /// The audio output (PulseAudio/PipeWire) failed.
    SinkFailed {
        message: String,
    },
    SessionLost,
    ReconnectFailed,
    SessionRestored,
    /// Time passed; sent at [`Supervisor::next_deadline`].
    Tick,
}

/// What the host must do in response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Send(Command),
    /// Stop the player now: the track cannot be decrypted, and librespot would
    /// otherwise play it undecrypted, which is silence.
    StopPlayer,
    /// Load this track again, at this position.
    Reload {
        track: TrackUri,
        position_ms: u32,
    },
    /// Build a new session (and Spirc) from the stored credentials.
    Reconnect,
    Notify(Notice),
}

/// Something worth telling the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// Spotify is throttling audio keys; playback resumes by itself.
    Throttled {
        retry_in_secs: u64,
    },
    /// Playback works again after a throttle, an output failure or a reconnect.
    Recovered,
    /// This account cannot play through librespot (PlayPlay DRM).
    Blocked,
    TrackUnavailable {
        track: TrackUri,
    },
    AudioOutputFailed {
        message: String,
    },
    ConnectionLost,
}

/// What the UI shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Loading {
        track: TrackUri,
    },
    Playing {
        track: TrackUri,
    },
    Paused {
        track: TrackUri,
    },
    /// Waiting out Spotify's key throttle before retrying.
    Throttled {
        track: TrackUri,
        retry_at: Millis,
    },
    /// `retry_at` is `None` while an attempt is in flight.
    Reconnecting {
        retry_at: Option<Millis>,
    },
    Blocked,
    OutputFailed {
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Waits after successive key refusals; the last one repeats.
    pub throttle_backoff_ms: Vec<u64>,
    /// Waits after successive failed reconnects; the last one repeats.
    pub reconnect_backoff_ms: Vec<u64>,
    /// Skips closer together than this are sent as one.
    pub skip_window_ms: u64,
    /// `Blocked` refusals in a row, before anything played, that mean PlayPlay.
    pub blocked_after: u32,
}

impl Default for Config {
    /// Tuned to spike S1: ~24 keys, then roughly one per 30–35 s.
    fn default() -> Config {
        Config {
            throttle_backoff_ms: vec![5_000, 15_000, 30_000],
            reconnect_backoff_ms: vec![1_000, 2_000, 5_000, 10_000, 30_000],
            skip_window_ms: 400,
            blocked_after: 2,
        }
    }
}

pub struct Supervisor {
    config: Config,
    status: Status,
    /// The track the engine last loaded, and where playback was in it.
    current: Option<TrackUri>,
    position_ms: u32,
    /// Set while playing, so the position can be estimated at any moment.
    playing_since: Option<Millis>,
    /// False once the player was stopped under us (throttle, reconnect):
    /// resuming then means loading the track again, not just pressing play.
    player_has_track: bool,
    /// Whether anything produced audio this session (rules out PlayPlay).
    played_ok: bool,
    blocked_streak: u32,
    /// Refusals in a row; indexes the throttle backoff.
    throttle_attempt: usize,
    /// A preload's key was refused: a load starting before this waits too.
    preload_cooldown_until: Option<Millis>,
    reconnect_attempt: usize,
    resume_after_reconnect: bool,
    /// A load or skip asked for while waiting, sent when the wait is over.
    deferred: Option<Command>,
    skip_window_until: Option<Millis>,
    pending_skip: i32,
}

impl Supervisor {
    pub fn new(config: Config) -> Supervisor {
        Supervisor {
            config,
            status: Status::Idle,
            current: None,
            position_ms: 0,
            playing_since: None,
            player_has_track: false,
            played_ok: false,
            blocked_streak: 0,
            throttle_attempt: 0,
            preload_cooldown_until: None,
            reconnect_attempt: 0,
            resume_after_reconnect: false,
            deferred: None,
            skip_window_until: None,
            pending_skip: 0,
        }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// The track and the place in it at `now` (counted on while playing), to
    /// pick up from after a restart.
    pub fn position(&self, now: Millis) -> Option<(TrackUri, u32)> {
        let track = self.current.clone()?;
        let running = self
            .playing_since
            .map_or(0, |since| now.0.saturating_sub(since.0) as u32);
        Some((track, self.position_ms.saturating_add(running)))
    }

    /// Takes up where an earlier run stopped: paused at `position_ms` in
    /// `track`, with nothing loaded yet, so Play loads it there.
    pub fn resume_at(&mut self, track: TrackUri, position_ms: u32) {
        self.current = Some(track.clone());
        self.position_ms = position_ms;
        self.playing_since = None;
        self.player_has_track = false;
        self.status = Status::Paused { track };
    }

    /// When the supervisor next needs an [`Input::Tick`], if at all.
    pub fn next_deadline(&self) -> Option<Millis> {
        let waiting = match self.status {
            Status::Throttled { retry_at, .. } => Some(retry_at),
            Status::Reconnecting { retry_at } => retry_at,
            _ => None,
        };
        [waiting, self.skip_window_until]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn handle(&mut self, now: Millis, input: Input) -> Vec<Effect> {
        match input {
            Input::User(command) => self.user(now, command),
            Input::Loading { track } => self.loading(now, track),
            Input::Playing { track, position_ms } => self.playing(now, track, position_ms),
            Input::Paused { track, position_ms } => {
                self.current = Some(track.clone());
                self.position_ms = position_ms;
                self.playing_since = None;
                // librespot pauses after a sink failure; keep showing the failure.
                if !matches!(self.status, Status::OutputFailed { .. }) {
                    self.status = Status::Paused { track };
                }
                vec![]
            }
            Input::Seeked { track, position_ms } => {
                self.current = Some(track);
                self.position_ms = position_ms;
                if self.playing_since.is_some() {
                    self.playing_since = Some(now);
                }
                vec![]
            }
            Input::Stopped => {
                self.playing_since = None;
                if matches!(
                    self.status,
                    Status::Loading { .. } | Status::Playing { .. } | Status::Paused { .. }
                ) {
                    self.status = Status::Idle;
                }
                vec![]
            }
            Input::Unavailable { track } => {
                if self.status
                    == (Status::Playing {
                        track: track.clone(),
                    })
                {
                    self.status = Status::Idle;
                }
                vec![Effect::Notify(Notice::TrackUnavailable { track })]
            }
            Input::KeyRefused(error) => self.key_refused(now, error),
            Input::SinkFailed { message } => {
                self.playing_since = None;
                self.status = Status::OutputFailed {
                    message: message.clone(),
                };
                vec![Effect::Notify(Notice::AudioOutputFailed { message })]
            }
            Input::SessionLost => self.session_lost(now),
            Input::ReconnectFailed => {
                let wait = backoff(&self.config.reconnect_backoff_ms, self.reconnect_attempt);
                self.reconnect_attempt += 1;
                self.status = Status::Reconnecting {
                    retry_at: Some(now + wait),
                };
                vec![]
            }
            Input::SessionRestored => self.session_restored(),
            Input::Tick => self.tick(now),
        }
    }

    fn user(&mut self, now: Millis, command: Command) -> Vec<Effect> {
        if matches!(
            command,
            Command::AddToQueue(_)
                | Command::Shuffle(_)
                | Command::Repeat(_)
                | Command::SetVolume(_)
        ) {
            // No key involved; only an engine that is gone cannot take it.
            return match self.status {
                Status::Reconnecting { .. } | Status::Blocked => vec![],
                _ => vec![Effect::Send(command)],
            };
        }
        match self.status.clone() {
            Status::Throttled { track, .. } => {
                match command {
                    Command::Skip(n) => {
                        self.deferred = match self.deferred.take() {
                            Some(Command::Skip(m)) => Some(Command::Skip(m + n)),
                            // a skip relative to a context not yet loaded means nothing
                            Some(load @ (Command::Load { .. } | Command::PlayTracks { .. })) => {
                                Some(load)
                            }
                            _ => Some(Command::Skip(n)),
                        };
                    }
                    Command::Load { .. } | Command::PlayTracks { .. } => {
                        self.deferred = Some(command)
                    }
                    Command::Pause => {
                        self.deferred = None;
                        self.player_has_track = false;
                        self.status = Status::Paused { track };
                    }
                    Command::Seek(ms) => self.position_ms = ms,
                    Command::Play
                    | Command::AddToQueue(_)
                    | Command::Shuffle(_)
                    | Command::Repeat(_)
                    | Command::SetVolume(_) => {} // the retry will play
                }
                vec![]
            }
            Status::Reconnecting { .. } => {
                match command {
                    Command::Load { .. } | Command::PlayTracks { .. } => {
                        self.deferred = Some(command)
                    }
                    Command::Pause => self.resume_after_reconnect = false,
                    Command::Play => self.resume_after_reconnect = true,
                    Command::Seek(ms) => self.position_ms = ms,
                    Command::Skip(_)
                    | Command::AddToQueue(_)
                    | Command::Shuffle(_)
                    | Command::Repeat(_)
                    | Command::SetVolume(_) => {}
                }
                vec![]
            }
            Status::Blocked => vec![],
            _ => match command {
                Command::Skip(n) => self.skip(now, n),
                Command::Play if !self.player_has_track => match self.current.clone() {
                    Some(track) => self.reload(track),
                    None => vec![Effect::Send(Command::Play)],
                },
                Command::Load { .. } | Command::PlayTracks { .. } => {
                    self.skip_window_until = None;
                    self.pending_skip = 0;
                    vec![Effect::Send(command)]
                }
                other => vec![Effect::Send(other)],
            },
        }
    }

    /// The first skip goes at once; skips right after it are added up and
    /// sent as one when they stop, so a burst costs two audio keys, not ten.
    fn skip(&mut self, now: Millis, n: i32) -> Vec<Effect> {
        let in_burst = self.skip_window_until.is_some_and(|until| now < until);
        self.skip_window_until = Some(now + self.config.skip_window_ms);
        if in_burst {
            self.pending_skip += n;
            vec![]
        } else {
            self.pending_skip = 0;
            vec![Effect::Send(Command::Skip(n))]
        }
    }

    fn loading(&mut self, now: Millis, track: TrackUri) -> Vec<Effect> {
        if self.current.as_ref() != Some(&track) {
            self.position_ms = 0;
        }
        self.current = Some(track.clone());
        self.playing_since = None;
        self.player_has_track = true;
        if let Some(until) = self.preload_cooldown_until.take()
            && now < until
        {
            return self.throttle(now, track);
        }
        self.status = Status::Loading { track };
        vec![]
    }

    fn playing(&mut self, now: Millis, track: TrackUri, position_ms: u32) -> Vec<Effect> {
        let recovered =
            self.throttle_attempt > 0 || matches!(self.status, Status::OutputFailed { .. });
        self.throttle_attempt = 0;
        self.blocked_streak = 0;
        self.played_ok = true;
        self.current = Some(track.clone());
        self.position_ms = position_ms;
        self.playing_since = Some(now);
        self.player_has_track = true;
        self.status = Status::Playing { track };
        if recovered {
            vec![Effect::Notify(Notice::Recovered)]
        } else {
            vec![]
        }
    }

    fn key_refused(&mut self, now: Millis, error: KeyError) -> Vec<Effect> {
        match error {
            KeyError::RateLimited => match self.status.clone() {
                Status::Loading { track } => self.throttle(now, track),
                Status::Playing { .. } => {
                    // The next track's preload; the current one keeps playing.
                    let wait = backoff(&self.config.throttle_backoff_ms, self.throttle_attempt);
                    self.preload_cooldown_until = Some(now + wait);
                    vec![]
                }
                _ => vec![],
            },
            KeyError::Blocked => {
                self.blocked_streak += 1;
                if !self.played_ok && self.blocked_streak >= self.config.blocked_after {
                    self.playing_since = None;
                    self.player_has_track = false;
                    self.status = Status::Blocked;
                    vec![Effect::StopPlayer, Effect::Notify(Notice::Blocked)]
                } else {
                    self.unavailable_current()
                }
            }
            KeyError::Other(..) => self.unavailable_current(),
        }
    }

    fn unavailable_current(&self) -> Vec<Effect> {
        match &self.current {
            Some(track) => vec![Effect::Notify(Notice::TrackUnavailable {
                track: track.clone(),
            })],
            None => vec![],
        }
    }

    fn throttle(&mut self, now: Millis, track: TrackUri) -> Vec<Effect> {
        let wait = backoff(&self.config.throttle_backoff_ms, self.throttle_attempt);
        self.throttle_attempt += 1;
        let retry_at = now + wait;
        self.playing_since = None;
        self.player_has_track = false;
        self.status = Status::Throttled { track, retry_at };
        vec![
            Effect::StopPlayer,
            Effect::Notify(Notice::Throttled {
                retry_in_secs: now.secs_until(retry_at),
            }),
        ]
    }

    fn reload(&mut self, track: TrackUri) -> Vec<Effect> {
        self.status = Status::Loading {
            track: track.clone(),
        };
        vec![Effect::Reload {
            track,
            position_ms: self.position_ms,
        }]
    }

    fn session_lost(&mut self, now: Millis) -> Vec<Effect> {
        if let Some(since) = self.playing_since.take() {
            self.position_ms = self.position_ms.saturating_add((now - since) as u32);
        }
        self.resume_after_reconnect = matches!(
            self.status,
            Status::Playing { .. } | Status::Loading { .. } | Status::Throttled { .. }
        );
        self.player_has_track = false;
        self.reconnect_attempt = 0;
        self.skip_window_until = None;
        self.pending_skip = 0;
        self.status = Status::Reconnecting { retry_at: None };
        vec![Effect::Notify(Notice::ConnectionLost), Effect::Reconnect]
    }

    fn session_restored(&mut self) -> Vec<Effect> {
        self.reconnect_attempt = 0;
        let mut effects = vec![Effect::Notify(Notice::Recovered)];
        if let Some(command) = self.deferred.take() {
            self.status = Status::Idle;
            effects.push(Effect::Send(command));
        } else if let Some(track) = self.current.clone() {
            if self.resume_after_reconnect {
                effects.extend(self.reload(track));
            } else {
                self.status = Status::Paused { track };
            }
        } else {
            self.status = Status::Idle;
        }
        effects
    }

    fn tick(&mut self, now: Millis) -> Vec<Effect> {
        let mut effects = vec![];
        if self.skip_window_until.is_some_and(|until| now >= until) {
            self.skip_window_until = None;
            let n = std::mem::take(&mut self.pending_skip);
            if n != 0 {
                effects.push(Effect::Send(Command::Skip(n)));
            }
        }
        match self.status.clone() {
            Status::Throttled { track, retry_at } if now >= retry_at => {
                match self.deferred.take() {
                    Some(command) => {
                        self.status = Status::Loading { track };
                        effects.push(Effect::Send(command));
                    }
                    None => effects.extend(self.reload(track)),
                }
            }
            Status::Reconnecting { retry_at: Some(at) } if now >= at => {
                self.status = Status::Reconnecting { retry_at: None };
                effects.push(Effect::Reconnect);
            }
            _ => {}
        }
        effects
    }
}

/// The wait for the given attempt; past the end, the last wait repeats.
fn backoff(schedule: &[u64], attempt: usize) -> u64 {
    schedule
        .get(attempt)
        .or(schedule.last())
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T1: &str = "spotify:track:1111111111111111111111";
    const T2: &str = "spotify:track:2222222222222222222222";
    const CTX: &str = "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M";

    fn sup() -> Supervisor {
        Supervisor::new(Config::default())
    }

    fn at(s: &mut Supervisor, ms: u64, input: Input) -> Vec<Effect> {
        s.handle(Millis(ms), input)
    }

    fn user(c: Command) -> Input {
        Input::User(c)
    }

    fn loading(t: &str) -> Input {
        Input::Loading { track: t.into() }
    }

    fn playing(t: &str, pos: u32) -> Input {
        Input::Playing {
            track: t.into(),
            position_ms: pos,
        }
    }

    fn load_ctx() -> Command {
        Command::Load {
            context: CTX.into(),
            start_track: None,
        }
    }

    /// Loads a context and has the engine report T1 playing at `ms`.
    fn playing_t1(s: &mut Supervisor, ms: u64) {
        at(s, ms, user(load_ctx()));
        at(s, ms, loading(T1));
        at(s, ms, playing(T1, 0));
    }

    /// Has a load of T1 refused with a rate-limit at `ms`.
    fn throttled_t1(s: &mut Supervisor, ms: u64) -> Vec<Effect> {
        at(s, ms, user(load_ctx()));
        at(s, ms, loading(T1));
        at(s, ms, Input::KeyRefused(KeyError::RateLimited))
    }

    // ---- no fake play ------------------------------------------------------

    #[test]
    fn a_load_is_forwarded_but_nothing_claims_to_play_yet() {
        let mut s = sup();
        assert_eq!(
            at(&mut s, 0, user(load_ctx())),
            vec![Effect::Send(load_ctx())]
        );
        assert_eq!(s.status(), &Status::Idle);
        at(&mut s, 10, loading(T1));
        assert_eq!(s.status(), &Status::Loading { track: T1.into() });
    }

    #[test]
    fn playing_only_once_the_engine_says_so() {
        let mut s = sup();
        at(&mut s, 0, user(load_ctx()));
        at(&mut s, 10, loading(T1));
        assert_eq!(at(&mut s, 300, playing(T1, 0)), vec![]);
        assert_eq!(s.status(), &Status::Playing { track: T1.into() });
    }

    #[test]
    fn play_pause_and_seek_go_straight_through() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        assert_eq!(
            at(&mut s, 1, user(Command::Pause)),
            vec![Effect::Send(Command::Pause)]
        );
        assert_eq!(
            at(&mut s, 2, user(Command::Play)),
            vec![Effect::Send(Command::Play)]
        );
        assert_eq!(
            at(&mut s, 3, user(Command::Seek(9_000))),
            vec![Effect::Send(Command::Seek(9_000))]
        );
    }

    #[test]
    fn paused_reports_are_shown() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        at(
            &mut s,
            5_000,
            Input::Paused {
                track: T1.into(),
                position_ms: 5_000,
            },
        );
        assert_eq!(s.status(), &Status::Paused { track: T1.into() });
    }

    // ---- key throttle ------------------------------------------------------

    #[test]
    fn a_rate_limited_key_stops_the_player_and_waits() {
        let mut s = sup();
        let effects = throttled_t1(&mut s, 1_000);
        assert_eq!(
            effects,
            vec![
                Effect::StopPlayer,
                Effect::Notify(Notice::Throttled { retry_in_secs: 5 })
            ]
        );
        assert_eq!(
            s.status(),
            &Status::Throttled {
                track: T1.into(),
                retry_at: Millis(6_000)
            }
        );
        assert_eq!(s.next_deadline(), Some(Millis(6_000)));
    }

    #[test]
    fn the_same_track_is_retried_when_the_wait_is_over() {
        let mut s = sup();
        throttled_t1(&mut s, 1_000);
        assert_eq!(at(&mut s, 5_999, Input::Tick), vec![]);
        assert_eq!(
            at(&mut s, 6_000, Input::Tick),
            vec![Effect::Reload {
                track: T1.into(),
                position_ms: 0
            }]
        );
        assert_eq!(s.status(), &Status::Loading { track: T1.into() });
    }

    #[test]
    fn waits_grow_and_then_hold() {
        let mut s = sup();
        throttled_t1(&mut s, 0);
        let mut now = 5_000;
        for expected in [15, 30, 30] {
            at(&mut s, now, Input::Tick); // retry
            at(&mut s, now, loading(T1));
            let effects = at(&mut s, now, Input::KeyRefused(KeyError::RateLimited));
            assert_eq!(
                effects[1],
                Effect::Notify(Notice::Throttled {
                    retry_in_secs: expected
                })
            );
            now += expected * 1_000;
        }
    }

    #[test]
    fn recovering_says_so_and_resets_the_wait() {
        let mut s = sup();
        throttled_t1(&mut s, 0);
        at(&mut s, 5_000, Input::Tick);
        at(&mut s, 5_000, loading(T1));
        assert_eq!(
            at(&mut s, 5_300, playing(T1, 0)),
            vec![Effect::Notify(Notice::Recovered)]
        );
        assert_eq!(s.status(), &Status::Playing { track: T1.into() });

        at(&mut s, 200_000, loading(T2));
        let effects = at(&mut s, 200_000, Input::KeyRefused(KeyError::RateLimited));
        assert_eq!(
            effects[1],
            Effect::Notify(Notice::Throttled { retry_in_secs: 5 })
        );
    }

    #[test]
    fn skips_while_throttled_are_collected_and_sent_once() {
        let mut s = sup();
        throttled_t1(&mut s, 0);
        for ms in [100, 200, 300] {
            assert_eq!(at(&mut s, ms, user(Command::Skip(1))), vec![]);
        }
        assert_eq!(
            at(&mut s, 5_000, Input::Tick),
            vec![Effect::Send(Command::Skip(3))]
        );
    }

    #[test]
    fn a_load_while_throttled_waits_and_replaces_collected_skips() {
        let mut s = sup();
        throttled_t1(&mut s, 0);
        at(&mut s, 100, user(Command::Skip(1)));
        let other = Command::Load {
            context: "spotify:album:3333333333333333333333".into(),
            start_track: None,
        };
        assert_eq!(at(&mut s, 200, user(other.clone())), vec![]);
        assert_eq!(at(&mut s, 5_000, Input::Tick), vec![Effect::Send(other)]);
    }

    #[test]
    fn pausing_while_throttled_cancels_the_retry() {
        let mut s = sup();
        throttled_t1(&mut s, 0);
        at(&mut s, 1_000, user(Command::Pause));
        assert_eq!(s.status(), &Status::Paused { track: T1.into() });
        assert_eq!(at(&mut s, 5_000, Input::Tick), vec![]);
        // Play then has to load the track: the player never had it.
        assert_eq!(
            at(&mut s, 60_000, user(Command::Play)),
            vec![Effect::Reload {
                track: T1.into(),
                position_ms: 0
            }]
        );
    }

    #[test]
    fn a_refused_preload_does_not_stop_the_track_that_plays() {
        // librespot fetches the next track's key near the end of the current one.
        let mut s = sup();
        playing_t1(&mut s, 0);
        assert_eq!(
            at(&mut s, 170_000, Input::KeyRefused(KeyError::RateLimited)),
            vec![]
        );
        assert_eq!(s.status(), &Status::Playing { track: T1.into() });
        // ...but when the next track starts loading inside that wait, it waits too.
        let effects = at(&mut s, 172_000, loading(T2));
        assert_eq!(
            effects,
            vec![
                Effect::StopPlayer,
                Effect::Notify(Notice::Throttled { retry_in_secs: 5 })
            ]
        );
        assert_eq!(
            s.status(),
            &Status::Throttled {
                track: T2.into(),
                retry_at: Millis(177_000)
            }
        );
    }

    #[test]
    fn a_refused_preload_is_forgotten_once_its_wait_has_passed() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        at(&mut s, 170_000, Input::KeyRefused(KeyError::RateLimited));
        assert_eq!(at(&mut s, 176_000, loading(T2)), vec![]);
        assert_eq!(s.status(), &Status::Loading { track: T2.into() });
    }

    // ---- skip bursts -------------------------------------------------------

    #[test]
    fn the_first_skip_is_immediate_and_a_burst_after_it_becomes_one() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        assert_eq!(
            at(&mut s, 1_000, user(Command::Skip(1))),
            vec![Effect::Send(Command::Skip(1))]
        );
        assert_eq!(at(&mut s, 1_100, user(Command::Skip(1))), vec![]);
        assert_eq!(at(&mut s, 1_200, user(Command::Skip(1))), vec![]);
        assert_eq!(s.next_deadline(), Some(Millis(1_600)));
        assert_eq!(at(&mut s, 1_599, Input::Tick), vec![]);
        assert_eq!(
            at(&mut s, 1_600, Input::Tick),
            vec![Effect::Send(Command::Skip(2))]
        );
        // a lone skip later is immediate again
        assert_eq!(
            at(&mut s, 5_000, user(Command::Skip(1))),
            vec![Effect::Send(Command::Skip(1))]
        );
    }

    #[test]
    fn skips_that_cancel_out_send_nothing() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        at(&mut s, 1_000, user(Command::Skip(1)));
        at(&mut s, 1_100, user(Command::Skip(1)));
        at(&mut s, 1_200, user(Command::Skip(-1)));
        assert_eq!(at(&mut s, 1_600, Input::Tick), vec![]);
        assert_eq!(s.next_deadline(), None);
    }

    // ---- PlayPlay ----------------------------------------------------------

    #[test]
    fn blocked_keys_before_anything_played_mean_playplay() {
        let mut s = sup();
        at(&mut s, 0, user(load_ctx()));
        at(&mut s, 0, loading(T1));
        assert_eq!(
            at(&mut s, 100, Input::KeyRefused(KeyError::Blocked)),
            vec![Effect::Notify(Notice::TrackUnavailable {
                track: T1.into()
            })]
        );
        at(&mut s, 200, loading(T2));
        assert_eq!(
            at(&mut s, 300, Input::KeyRefused(KeyError::Blocked)),
            vec![Effect::StopPlayer, Effect::Notify(Notice::Blocked)]
        );
        assert_eq!(s.status(), &Status::Blocked);
    }

    #[test]
    fn once_something_played_a_blocked_key_is_just_that_track() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        for (ms, t) in [(1_000, T2), (2_000, T1)] {
            at(&mut s, ms, loading(t));
            assert_eq!(
                at(&mut s, ms, Input::KeyRefused(KeyError::Blocked)),
                vec![Effect::Notify(Notice::TrackUnavailable { track: t.into() })]
            );
        }
        assert_ne!(s.status(), &Status::Blocked);
    }

    #[test]
    fn unavailable_tracks_are_reported_not_played() {
        let mut s = sup();
        at(&mut s, 0, user(load_ctx()));
        at(&mut s, 0, loading(T1));
        assert_eq!(
            at(&mut s, 100, Input::Unavailable { track: T1.into() }),
            vec![Effect::Notify(Notice::TrackUnavailable {
                track: T1.into()
            })]
        );
        assert_ne!(s.status(), &Status::Playing { track: T1.into() });
    }

    // ---- reconnecting ------------------------------------------------------

    #[test]
    fn a_lost_session_reconnects_at_once_and_then_backs_off() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        assert_eq!(
            at(&mut s, 10_000, Input::SessionLost),
            vec![Effect::Notify(Notice::ConnectionLost), Effect::Reconnect]
        );
        assert_eq!(s.status(), &Status::Reconnecting { retry_at: None });

        at(&mut s, 10_500, Input::ReconnectFailed);
        assert_eq!(
            s.status(),
            &Status::Reconnecting {
                retry_at: Some(Millis(11_500))
            }
        );
        assert_eq!(at(&mut s, 11_500, Input::Tick), vec![Effect::Reconnect]);
        at(&mut s, 12_000, Input::ReconnectFailed);
        assert_eq!(
            s.status(),
            &Status::Reconnecting {
                retry_at: Some(Millis(14_000))
            }
        );
    }

    #[test]
    fn a_restored_session_picks_up_where_playback_was() {
        let mut s = sup();
        at(&mut s, 0, user(load_ctx()));
        at(&mut s, 0, loading(T1));
        at(&mut s, 1_000, playing(T1, 10_000));
        at(&mut s, 31_000, Input::SessionLost);
        assert_eq!(
            at(&mut s, 32_000, Input::SessionRestored),
            vec![
                Effect::Notify(Notice::Recovered),
                Effect::Reload {
                    track: T1.into(),
                    position_ms: 40_000
                }
            ]
        );
        assert_eq!(s.status(), &Status::Loading { track: T1.into() });
    }

    #[test]
    fn a_paused_track_stays_paused_after_reconnecting() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        at(
            &mut s,
            5_000,
            Input::Paused {
                track: T1.into(),
                position_ms: 5_000,
            },
        );
        at(&mut s, 60_000, Input::SessionLost);
        assert_eq!(
            at(&mut s, 61_000, Input::SessionRestored),
            vec![Effect::Notify(Notice::Recovered)]
        );
        assert_eq!(s.status(), &Status::Paused { track: T1.into() });
        assert_eq!(
            at(&mut s, 70_000, user(Command::Play)),
            vec![Effect::Reload {
                track: T1.into(),
                position_ms: 5_000
            }]
        );
    }

    #[test]
    fn a_load_asked_for_while_reconnecting_is_sent_after() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        at(&mut s, 1_000, Input::SessionLost);
        assert_eq!(at(&mut s, 1_100, user(load_ctx())), vec![]);
        assert_eq!(
            at(&mut s, 2_000, Input::SessionRestored),
            vec![Effect::Notify(Notice::Recovered), Effect::Send(load_ctx())]
        );
    }

    // ---- audio output ------------------------------------------------------

    #[test]
    fn an_output_failure_is_shown_until_audio_flows_again() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        assert_eq!(
            at(
                &mut s,
                1_000,
                Input::SinkFailed {
                    message: "Connection refused".into()
                }
            ),
            vec![Effect::Notify(Notice::AudioOutputFailed {
                message: "Connection refused".into()
            })]
        );
        assert_eq!(
            s.status(),
            &Status::OutputFailed {
                message: "Connection refused".into()
            }
        );
        assert_eq!(
            at(&mut s, 5_000, playing(T1, 1_000)),
            vec![Effect::Notify(Notice::Recovered)]
        );
        assert_eq!(s.status(), &Status::Playing { track: T1.into() });
    }

    #[test]
    fn the_pause_that_follows_an_output_failure_does_not_hide_it() {
        // librespot pauses the player when the sink fails to start or write.
        let mut s = sup();
        playing_t1(&mut s, 0);
        at(
            &mut s,
            1_000,
            Input::SinkFailed {
                message: "no server".into(),
            },
        );
        at(
            &mut s,
            1_001,
            Input::Paused {
                track: T1.into(),
                position_ms: 1_000,
            },
        );
        assert_eq!(
            s.status(),
            &Status::OutputFailed {
                message: "no server".into()
            }
        );
        // pressing play tries the output again
        assert_eq!(
            at(&mut s, 9_000, user(Command::Play)),
            vec![Effect::Send(Command::Play)]
        );
    }

    #[test]
    fn adding_to_the_queue_goes_straight_through_even_while_throttled() {
        let mut s = sup();
        let add = Command::AddToQueue(T2.into());
        playing_t1(&mut s, 0);
        assert_eq!(
            at(&mut s, 1, user(add.clone())),
            vec![Effect::Send(add.clone())]
        );
        let mut t = sup();
        throttled_t1(&mut t, 0);
        assert_eq!(at(&mut t, 1, user(add.clone())), vec![Effect::Send(add)]);
    }

    #[test]
    fn shuffle_and_repeat_go_straight_through() {
        let mut s = sup();
        playing_t1(&mut s, 0);
        let shuffle = Command::Shuffle(true);
        let repeat = Command::Repeat(Repeat::Track);
        assert_eq!(
            at(&mut s, 1, user(shuffle.clone())),
            vec![Effect::Send(shuffle)]
        );
        assert_eq!(
            at(&mut s, 2, user(repeat.clone())),
            vec![Effect::Send(repeat)]
        );
    }

    #[test]
    fn the_repeat_button_cycles_off_context_track() {
        assert_eq!(Repeat::Off.next(), Repeat::Context);
        assert_eq!(Repeat::Context.next(), Repeat::Track);
        assert_eq!(Repeat::Track.next(), Repeat::Off);
    }

    #[test]
    fn a_track_list_plays_like_a_context_and_waits_like_one() {
        let play = Command::PlayTracks {
            tracks: vec![T1.into(), T2.into()],
            start: 1,
            position_ms: 0,
        };
        let mut s = sup();
        assert_eq!(
            at(&mut s, 0, user(play.clone())),
            vec![Effect::Send(play.clone())]
        );
        let mut t = sup();
        throttled_t1(&mut t, 0);
        assert_eq!(at(&mut t, 100, user(play.clone())), vec![]);
        assert_eq!(at(&mut t, 5_000, Input::Tick), vec![Effect::Send(play)]);
    }

    #[test]
    fn volume_goes_straight_through() {
        let mut s = sup();
        let set = Command::SetVolume(20_000);
        assert_eq!(
            at(&mut s, 0, user(set.clone())),
            vec![Effect::Send(set.clone())]
        );
        let mut t = sup();
        throttled_t1(&mut t, 0);
        assert_eq!(at(&mut t, 1, user(set.clone())), vec![Effect::Send(set)]);
    }

    // ---- picking up after a restart ----

    #[test]
    fn an_earlier_run_comes_back_paused_and_play_loads_it_at_its_place() {
        let mut s = sup();
        s.resume_at(T1.into(), 42_000);
        assert_eq!(s.status(), &Status::Paused { track: T1.into() });
        assert_eq!(
            at(&mut s, 0, user(Command::Play)),
            vec![Effect::Reload {
                track: T1.into(),
                position_ms: 42_000
            }]
        );
        assert_eq!(s.status(), &Status::Loading { track: T1.into() });
    }

    #[test]
    fn the_place_runs_on_while_playing_and_stands_still_when_paused() {
        let mut s = sup();
        assert_eq!(s.position(Millis(0)), None);
        at(&mut s, 1_000, playing(T1, 0));
        assert_eq!(s.position(Millis(6_000)), Some((T1.into(), 5_000)));
        at(
            &mut s,
            10_000,
            Input::Paused {
                track: T1.into(),
                position_ms: 9_000,
            },
        );
        assert_eq!(s.position(Millis(60_000)), Some((T1.into(), 9_000)));
    }

    #[test]
    fn a_seek_from_elsewhere_moves_the_place() {
        let mut s = sup();
        at(&mut s, 0, playing(T1, 0));
        at(
            &mut s,
            2_000,
            Input::Seeked {
                track: T1.into(),
                position_ms: 100_000,
            },
        );
        assert_eq!(s.position(Millis(3_000)), Some((T1.into(), 101_000)));
        // paused, a seek moves the place but does not start the clock
        at(
            &mut s,
            4_000,
            Input::Paused {
                track: T1.into(),
                position_ms: 102_000,
            },
        );
        at(
            &mut s,
            5_000,
            Input::Seeked {
                track: T1.into(),
                position_ms: 30_000,
            },
        );
        assert_eq!(s.position(Millis(50_000)), Some((T1.into(), 30_000)));
    }
}
