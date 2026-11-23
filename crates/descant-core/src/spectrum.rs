//! What the visualizer draws from: the sound just heard, as a handful of
//! smoothed frequency bands, three overall levels, beats and the waveform.
//!
//! [`Analyzer::update`] takes the last [`WINDOW`] mono samples and the time
//! since the previous call, so the smoothing runs at the same speed whatever
//! the display's refresh rate.

use std::f32::consts::{PI, TAU};

/// Samples per analysis: 46 ms at 44.1 kHz, about 21 Hz per bin.
pub const WINDOW: usize = 2048;
/// Frequency bands, spaced evenly in octaves from [`LOWEST_HZ`] up.
pub const BANDS: usize = 32;
/// Points in the waveform; each averages `WINDOW / WAVE` samples.
pub const WAVE: usize = 128;

const LOWEST_HZ: f32 = 40.0;
const HIGHEST_HZ: f32 = 16_000.0;
/// The loudness a band reads 0 at and 1 at, in dB of a full-scale sine.
const FLOOR_DB: f32 = -62.0;
const CEILING_DB: f32 = -8.0;
/// Music carries less energy the higher it goes; this much per octave above
/// 1 kHz is added back (and taken off below), so the bands read alike.
const TILT_DB_PER_OCTAVE: f32 = 3.0;
/// How quickly a band follows a louder and a quieter sound, in seconds.
const ATTACK_S: f32 = 0.03;
const RELEASE_S: f32 = 0.22;
/// A beat is bass this much above its recent average, at most this often.
const BEAT_RATIO: f32 = 1.3;
const BEAT_MARGIN: f32 = 0.06;
const BEAT_GAP_S: f32 = 0.25;
/// ...and has just risen this much above the last tenth of a second, so a
/// held note is one beat and not one every quarter second.
const BEAT_RISE: f32 = 1.15;
/// How long the bass average remembers, and how long a beat glows.
const BASS_MEMORY_S: f32 = 1.2;
const BASS_RECENT_S: f32 = 0.1;
const BEAT_FADE_S: f32 = 0.3;

/// What one moment sounds like, everything in 0–1 (the waveform in −1–1).
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub bands: [f32; BANDS],
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    /// 1 at a beat, fading to 0.
    pub beat: f32,
    /// Set on the update a beat starts.
    pub onset: bool,
    pub wave: [f32; WAVE],
}

impl Default for Frame {
    fn default() -> Frame {
        Frame {
            bands: [0.0; BANDS],
            bass: 0.0,
            mid: 0.0,
            treble: 0.0,
            beat: 0.0,
            onset: false,
            wave: [0.0; WAVE],
        }
    }
}

impl Frame {
    /// How much is going on overall.
    pub fn energy(&self) -> f32 {
        (self.bass + self.mid + self.treble) / 3.0
    }
}

pub struct Analyzer {
    fft: Fft,
    window: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    /// The first bin of each band, and one past the last band.
    edges: [usize; BANDS + 1],
    /// Decibels added to each band to even out the tilt.
    tilt: [f32; BANDS],
    frame: Frame,
    bass_average: f32,
    bass_recent: f32,
    since_beat: f32,
}

impl Analyzer {
    pub fn new(sample_rate: u32) -> Analyzer {
        let bin_hz = sample_rate as f32 / WINDOW as f32;
        let octaves = (HIGHEST_HZ / LOWEST_HZ).log2();
        let mut edges = [0; BANDS + 1];
        let mut tilt = [0.0; BANDS];
        for (b, edge) in edges.iter_mut().enumerate() {
            let hz = LOWEST_HZ * 2f32.powf(octaves * b as f32 / BANDS as f32);
            *edge = ((hz / bin_hz).round() as usize).clamp(1, WINDOW / 2);
        }
        // every band at least one bin wide
        for b in 1..=BANDS {
            edges[b] = edges[b].max(edges[b - 1] + 1).min(WINDOW / 2);
        }
        for (b, t) in tilt.iter_mut().enumerate() {
            let centre = LOWEST_HZ * 2f32.powf(octaves * (b as f32 + 0.5) / BANDS as f32);
            *t = TILT_DB_PER_OCTAVE * (centre / 1000.0).log2();
        }
        let window = (0..WINDOW)
            .map(|i| 0.5 - 0.5 * (TAU * i as f32 / (WINDOW - 1) as f32).cos())
            .collect();
        Analyzer {
            fft: Fft::new(WINDOW),
            window,
            re: vec![0.0; WINDOW],
            im: vec![0.0; WINDOW],
            edges,
            tilt,
            frame: Frame::default(),
            bass_average: 0.0,
            bass_recent: 0.0,
            since_beat: BEAT_GAP_S,
        }
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// Takes in the last [`WINDOW`] samples (fewer are padded with silence),
    /// `dt` seconds after the previous update.
    pub fn update(&mut self, samples: &[f32], dt: f32) -> &Frame {
        let dt = dt.clamp(0.0, 0.5);
        let samples = &samples[samples.len().saturating_sub(WINDOW)..];
        let pad = WINDOW - samples.len();
        for i in 0..WINDOW {
            let s = if i < pad { 0.0 } else { samples[i - pad] };
            self.re[i] = s * self.window[i];
            self.im[i] = 0.0;
        }
        self.fft.run(&mut self.re, &mut self.im);

        // A full-scale sine peaks at WINDOW / 4 through the Hann window.
        let scale = 4.0 / WINDOW as f32;
        let attack = 1.0 - (-dt / ATTACK_S).exp();
        let release = 1.0 - (-dt / RELEASE_S).exp();
        let mut raw = [0.0; BANDS];
        for (b, (level, band)) in raw.iter_mut().zip(&mut self.frame.bands).enumerate() {
            let (lo, hi) = (self.edges[b], self.edges[b + 1]);
            let peak = (lo..hi)
                .map(|k| self.re[k].hypot(self.im[k]))
                .fold(0.0f32, f32::max);
            let db = 20.0 * (peak * scale).max(1e-9).log10() + self.tilt[b];
            *level = ((db - FLOOR_DB) / (CEILING_DB - FLOOR_DB)).clamp(0.0, 1.0);
            let rate = if *level > *band { attack } else { release };
            *band += (*level - *band) * rate;
        }

        let mean = |range: std::ops::Range<usize>, of: &[f32; BANDS]| {
            let n = range.len() as f32;
            range.map(|b| of[b]).sum::<f32>() / n
        };
        let (bass, mid, treble) = (0..6, 6..19, 19..BANDS);
        self.frame.bass = mean(bass.clone(), &self.frame.bands);
        self.frame.mid = mean(mid, &self.frame.bands);
        self.frame.treble = mean(treble, &self.frame.bands);

        // Beats: the bass as it is now, against what it has been lately.
        let now = mean(bass, &raw);
        self.since_beat += dt;
        self.frame.onset = now > self.bass_average * BEAT_RATIO + BEAT_MARGIN
            && now > self.bass_recent * BEAT_RISE + BEAT_MARGIN / 2.0
            && self.since_beat >= BEAT_GAP_S;
        if self.frame.onset {
            self.since_beat = 0.0;
            self.frame.beat = 1.0;
        } else {
            self.frame.beat *= (-dt / BEAT_FADE_S).exp();
        }
        self.bass_average += (now - self.bass_average) * (1.0 - (-dt / BASS_MEMORY_S).exp());
        self.bass_recent += (now - self.bass_recent) * (1.0 - (-dt / BASS_RECENT_S).exp());

        let step = WINDOW / WAVE;
        for (j, point) in self.frame.wave.iter_mut().enumerate() {
            let start = j * step;
            let sum: f32 = (start..start + step)
                .map(|i| if i < pad { 0.0 } else { samples[i - pad] })
                .sum();
            *point = (sum / step as f32).clamp(-1.0, 1.0);
        }
        &self.frame
    }
}

/// An in-place radix-2 FFT of a fixed power-of-two size.
struct Fft {
    cos: Vec<f32>,
    sin: Vec<f32>,
    reversed: Vec<usize>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two());
        let bits = n.trailing_zeros();
        Fft {
            cos: (0..n / 2)
                .map(|k| (2.0 * PI * k as f32 / n as f32).cos())
                .collect(),
            sin: (0..n / 2)
                .map(|k| -(2.0 * PI * k as f32 / n as f32).sin())
                .collect(),
            reversed: (0..n)
                .map(|i| i.reverse_bits() >> (usize::BITS - bits))
                .collect(),
        }
    }

    fn run(&self, re: &mut [f32], im: &mut [f32]) {
        let n = re.len();
        for (i, &j) in self.reversed.iter().enumerate() {
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut size = 2;
        while size <= n {
            let half = size / 2;
            let stride = n / size;
            for start in (0..n).step_by(size) {
                for k in 0..half {
                    let (c, s) = (self.cos[k * stride], self.sin[k * stride]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            size *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    fn sine(hz: f32, amplitude: f32, offset: usize) -> Vec<f32> {
        (offset..offset + WINDOW)
            .map(|i| amplitude * (TAU * hz * i as f32 / RATE as f32).sin())
            .collect()
    }

    /// Updates at 60 per second for `seconds`.
    fn run(a: &mut Analyzer, seconds: f32, samples: impl Fn(usize) -> Vec<f32>) {
        let frames = (seconds * 60.0) as usize;
        for f in 0..frames {
            a.update(&samples(f * RATE as usize / 60), 1.0 / 60.0);
        }
    }

    #[test]
    fn the_fft_matches_a_plain_dft() {
        let n = 16;
        let fft = Fft::new(n);
        let input: Vec<f32> = (0..n).map(|i| ((i * 7) % 5) as f32 - 2.0).collect();
        let (mut re, mut im) = (input.clone(), vec![0.0; n]);
        fft.run(&mut re, &mut im);
        for k in 0..n {
            let (mut dr, mut di) = (0.0f32, 0.0f32);
            for (i, x) in input.iter().enumerate() {
                let angle = -TAU * (k * i) as f32 / n as f32;
                dr += x * angle.cos();
                di += x * angle.sin();
            }
            assert!((re[k] - dr).abs() < 1e-4, "re[{k}]");
            assert!((im[k] - di).abs() < 1e-4, "im[{k}]");
        }
    }

    #[test]
    fn bands_cover_the_range_in_order_without_gaps() {
        let a = Analyzer::new(RATE);
        assert!(a.edges.windows(2).all(|w| w[0] < w[1]));
        assert!(a.edges[0] >= 1, "no DC");
        assert!(a.edges[BANDS] <= WINDOW / 2);
    }

    #[test]
    fn silence_reads_nothing() {
        let mut a = Analyzer::new(RATE);
        run(&mut a, 1.0, |_| vec![0.0; WINDOW]);
        let f = a.frame();
        assert!(f.bands.iter().all(|&b| b == 0.0));
        assert_eq!(f.energy(), 0.0);
        assert_eq!(f.beat, 0.0);
    }

    #[test]
    fn a_tone_lights_its_own_band() {
        let mut a = Analyzer::new(RATE);
        run(&mut a, 0.5, |o| sine(1000.0, 0.5, o));
        let f = a.frame();
        let loudest = (0..BANDS)
            .max_by(|&x, &y| f.bands[x].total_cmp(&f.bands[y]))
            .unwrap();
        let (lo, hi) = (a.edges[loudest], a.edges[loudest + 1]);
        let bin_hz = RATE as f32 / WINDOW as f32;
        assert!(lo as f32 * bin_hz <= 1030.0 && 1000.0 <= hi as f32 * bin_hz);
        assert!(f.bands[loudest] > 0.8);
        assert!(f.bands[0] < 0.2 && f.bands[BANDS - 1] < 0.2);
        assert!(f.mid > f.bass && f.mid > f.treble);
    }

    #[test]
    fn bands_rise_fast_and_fall_slowly() {
        let mut a = Analyzer::new(RATE);
        a.update(&sine(1000.0, 0.5, 0), 1.0 / 60.0);
        let risen = a.frame().mid;
        a.update(&vec![0.0; WINDOW], 1.0 / 60.0);
        let fallen = a.frame().mid;
        assert!(risen > 0.0);
        assert!(fallen > risen * 0.7, "still most of the way up");
        run(&mut a, 2.0, |_| vec![0.0; WINDOW]);
        assert!(a.frame().mid < 0.01);
    }

    #[test]
    fn a_kick_after_quiet_is_a_beat_and_a_steady_tone_is_not() {
        let mut a = Analyzer::new(RATE);
        run(&mut a, 4.0, |o| sine(60.0, 0.02, o));
        assert!(a.frame().beat < 0.05);
        a.update(&sine(60.0, 0.8, 0), 1.0 / 60.0);
        assert!(a.frame().onset);
        assert_eq!(a.frame().beat, 1.0);
        // the same loudness held is no new beat
        let mut onsets = 0;
        for f in 1..120 {
            a.update(&sine(60.0, 0.8, f * 735), 1.0 / 60.0);
            onsets += usize::from(a.frame().onset);
        }
        assert_eq!(onsets, 0);
        assert!(a.frame().beat < 0.01, "faded");
    }

    #[test]
    fn a_kick_drum_at_120_bpm_is_a_beat_each_time() {
        let mut a = Analyzer::new(RATE);
        let mut onsets = 0;
        // eight bars of a 100 ms kick every half second, at 60 updates/s
        for f in 0..(60 * 4) {
            let in_beat = f % 30;
            let amplitude = if in_beat < 6 { 0.8 } else { 0.03 };
            a.update(&sine(55.0, amplitude, f * 735), 1.0 / 60.0);
            onsets += usize::from(a.frame().onset);
        }
        assert!((7..=9).contains(&onsets), "{onsets} beats for 8 kicks");
    }

    #[test]
    fn the_waveform_follows_the_samples() {
        let mut a = Analyzer::new(RATE);
        a.update(&vec![0.25; WINDOW], 1.0 / 60.0);
        assert!(a.frame().wave.iter().all(|&w| (w - 0.25).abs() < 1e-6));
        // too few samples: the start is silence
        a.update(&[0.5; WINDOW / 2], 1.0 / 60.0);
        assert_eq!(a.frame().wave[0], 0.0);
        assert_eq!(a.frame().wave[WAVE - 1], 0.5);
    }
}
