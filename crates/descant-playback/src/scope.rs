//! What the visualizer listens to: the sound as it reaches the speakers.
//!
//! [`crate::pulse::PulseSink`] hands every batch it writes to the server to a
//! [`Scope`], with the server's latency at that moment: how long until the
//! last sample written is heard. The visualizer asks for the samples heard
//! up to now, so what it shows lines up with what plays, and not with what
//! was decoded up to half a second earlier.
//!
//! Nobody listening costs nothing: the sink checks [`Scope::listening`]
//! first, and the buffer is only there while someone is.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use librespot_playback::NUM_CHANNELS;
/// The rate everything plays at; librespot decodes to this.
pub use librespot_playback::SAMPLE_RATE;

/// Mono frames kept: 0.74 s, enough for the server's queue and a window.
const KEPT: usize = 1 << 15;

#[derive(Default)]
pub struct Scope {
    listening: AtomicBool,
    heard: Mutex<Heard>,
}

#[derive(Default)]
struct Heard {
    /// The last [`KEPT`] frames written, as a ring indexed by frame number.
    ring: Vec<f32>,
    /// Frames written so far.
    written: u64,
    /// When the last batch was written, and how many of the frames written
    /// by then were still to be heard.
    last_write: Option<(Instant, u64)>,
}

impl Scope {
    pub fn new() -> Arc<Scope> {
        Arc::default()
    }

    /// Starts or stops keeping what is played. Stopping lets go of it all.
    pub fn listen(&self, on: bool) {
        self.listening.store(on, Ordering::Relaxed);
        if !on {
            *self.heard.lock().unwrap() = Heard::default();
        }
    }

    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::Relaxed)
    }

    /// Keeps a batch just written to the server: native-endian 32-bit float
    /// frames, [`NUM_CHANNELS`] samples each, the last of them heard after
    /// `latency`.
    pub fn feed(&self, bytes: &[u8], latency: Duration, now: Instant) {
        if !self.listening() {
            return;
        }
        let mut heard = self.heard.lock().unwrap();
        if heard.ring.is_empty() {
            heard.ring = vec![0.0; KEPT];
        }
        let channels = usize::from(NUM_CHANNELS);
        let (samples, _) = bytes.as_chunks::<4>();
        for frame in samples.chunks_exact(channels) {
            let sum: f32 = frame.iter().map(|s| f32::from_ne_bytes(*s)).sum();
            let at = (heard.written % KEPT as u64) as usize;
            heard.ring[at] = sum / channels as f32;
            heard.written += 1;
        }
        let ahead = (latency.as_secs_f64() * f64::from(SAMPLE_RATE)) as u64;
        heard.last_write = Some((now, ahead.min(heard.written)));
    }

    /// Fills `out` with the frames heard last before `now`, oldest first.
    /// False, and silence, once everything written has been heard.
    pub fn heard(&self, now: Instant, out: &mut [f32]) -> bool {
        out.fill(0.0);
        let heard = self.heard.lock().unwrap();
        let Some((at, ahead)) = heard.last_write else {
            return false;
        };
        let since = now.saturating_duration_since(at).as_secs_f64();
        let played = (since * f64::from(SAMPLE_RATE)) as u64;
        // The frame being heard now, one past the last in `out`.
        let end = heard.written - ahead + played;
        let n = out.len() as u64;
        if end >= heard.written + n {
            return false;
        }
        let oldest_kept = heard.written.saturating_sub(KEPT as u64);
        for (i, sample) in out.iter_mut().enumerate() {
            let Some(frame) = (end + i as u64).checked_sub(n) else {
                continue;
            };
            if frame >= oldest_kept && frame < heard.written {
                *sample = heard.ring[(frame % KEPT as u64) as usize];
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames of `value` on both channels, as the sink writes them.
    fn frames(values: impl IntoIterator<Item = f32>) -> Vec<u8> {
        values
            .into_iter()
            .flat_map(|v| [v, v])
            .flat_map(f32::to_ne_bytes)
            .collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn nothing_is_kept_unless_someone_listens() {
        let scope = Scope::new();
        let now = Instant::now();
        scope.feed(&frames([1.0; 441]), ms(0), now);
        let mut out = [9.0; 4];
        assert!(!scope.heard(now, &mut out));
        assert_eq!(out, [0.0; 4]);
        assert!(scope.heard.lock().unwrap().ring.is_empty());
    }

    #[test]
    fn what_is_heard_is_what_was_written_a_latency_ago() {
        let scope = Scope::new();
        scope.listen(true);
        let t0 = Instant::now();
        // 100 ms of 0.1 then 100 ms of 0.2, heard 200 ms after writing.
        let mut batch = vec![0.1; 4410];
        batch.extend([0.2; 4410]);
        scope.feed(&frames(batch), ms(200), t0);
        let mut out = [0.0; 4];
        // at t0 the first frame is about to be heard: nothing yet
        assert!(scope.heard(t0, &mut out));
        assert_eq!(out, [0.0; 4]);
        assert!(scope.heard(t0 + ms(50), &mut out));
        assert_eq!(out, [0.1; 4]);
        assert!(scope.heard(t0 + ms(150), &mut out));
        assert_eq!(out, [0.2; 4]);
    }

    #[test]
    fn the_two_channels_are_mixed_to_one() {
        let scope = Scope::new();
        scope.listen(true);
        let now = Instant::now();
        let bytes: Vec<u8> = [0.5f32, -0.1]
            .repeat(10)
            .into_iter()
            .flat_map(f32::to_ne_bytes)
            .collect();
        scope.feed(&bytes, ms(0), now);
        let mut out = [0.0; 2];
        scope.heard(now, &mut out);
        assert!(out.iter().all(|&s| (s - 0.2).abs() < 1e-6));
    }

    #[test]
    fn once_everything_written_is_heard_there_is_silence() {
        let scope = Scope::new();
        scope.listen(true);
        let t0 = Instant::now();
        scope.feed(&frames([0.3; 441]), ms(10), t0);
        let mut out = [0.0; 441];
        assert!(scope.heard(t0 + ms(10), &mut out));
        assert!(out.iter().all(|&s| s == 0.3));
        // the window is past the end
        assert!(!scope.heard(t0 + ms(30), &mut out));
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn stopping_lets_go_of_the_buffer() {
        let scope = Scope::new();
        scope.listen(true);
        scope.feed(&frames([0.3; 10]), ms(0), Instant::now());
        scope.listen(false);
        assert!(scope.heard.lock().unwrap().ring.is_empty());
        let mut out = [1.0; 2];
        assert!(!scope.heard(Instant::now(), &mut out));
    }
}
