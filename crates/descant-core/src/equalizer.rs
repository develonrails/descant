//! A ten-band equalizer for what plays on this computer.
//!
//! Ten peaking filters (the RBJ cookbook biquad) at the classic ten-band
//! frequencies, and a pre-amp. Boosting a band can push a full-scale track
//! past 0 dBFS, so the peak of the whole curve (neighbouring bands add up)
//! is taken off the signal first: a curve changes the balance, it does not
//! make the output clip.
//! Bands left at 0 dB cost nothing; an equalizer that is off is never called.

/// The centre of each band, in Hz.
pub const BANDS: [f64; 10] = [
    60.0, 170.0, 310.0, 600.0, 1_000.0, 3_000.0, 6_000.0, 12_000.0, 14_000.0, 16_000.0,
];
/// How far a band or the pre-amp moves either way, in dB.
pub const MAX_GAIN_DB: f64 = 12.0;
/// Bandwidth of each filter: about an octave.
const Q: f64 = 1.41;

/// Gains for the ten bands and the pre-amp, in dB.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Curve {
    pub preamp: f64,
    pub gains: [f64; 10],
}

impl Curve {
    pub fn is_flat(&self) -> bool {
        self.preamp == 0.0 && self.gains.iter().all(|g| *g == 0.0)
    }
}

/// A named set of band gains.
pub struct Preset {
    pub name: &'static str,
    pub gains: [f64; 10],
}

/// Flat first; the rest are the usual names for the usual curves.
pub const PRESETS: &[Preset] = &[
    preset("Flat", [0.0; 10]),
    preset(
        "Classical",
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -4.0, -4.0, -4.0, -6.0],
    ),
    preset("Club", [0.0, 0.0, 5.0, 3.0, 3.0, 3.0, 2.0, 0.0, 0.0, 0.0]),
    preset(
        "Dance",
        [6.0, 4.5, 1.5, 0.0, 0.0, -3.5, -4.5, -4.5, 0.0, 0.0],
    ),
    preset(
        "Full Bass",
        [6.0, 6.0, 6.0, 3.5, 1.0, -2.5, -5.0, -6.0, -6.5, -6.5],
    ),
    preset(
        "Full Bass & Treble",
        [4.5, 3.5, 0.0, -4.5, -3.0, 1.0, 5.0, 7.0, 7.5, 7.5],
    ),
    preset(
        "Full Treble",
        [-6.0, -6.0, -6.0, -2.5, 1.5, 7.0, 10.0, 10.0, 10.0, 10.5],
    ),
    preset(
        "Laptop Speakers / Headphones",
        [3.0, 7.0, 3.5, -2.0, -1.5, 1.0, 3.0, 6.0, 8.0, 9.0],
    ),
    preset(
        "Large Hall",
        [6.5, 6.5, 3.5, 3.5, 0.0, -3.0, -3.0, -3.0, 0.0, 0.0],
    ),
    preset("Live", [-3.0, 0.0, 2.5, 3.5, 3.5, 3.5, 2.5, 1.5, 1.5, 1.5]),
    preset("Party", [4.5, 4.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 4.5, 4.5]),
    preset(
        "Pop",
        [-1.0, 3.0, 4.5, 5.0, 3.5, 0.0, -1.5, -1.5, -1.0, -1.0],
    ),
    preset(
        "Reggae",
        [0.0, 0.0, 0.0, -3.5, 0.0, 4.0, 4.0, 0.0, 0.0, 0.0],
    ),
    preset(
        "Rock",
        [5.0, 3.0, -3.5, -5.0, -2.0, 2.5, 5.5, 7.0, 7.0, 7.0],
    ),
    preset("Ska", [-1.5, -3.0, -2.5, 0.0, 2.5, 3.5, 5.5, 6.0, 7.0, 6.0]),
    preset("Soft", [3.0, 1.0, 0.0, -1.5, 0.0, 2.5, 5.0, 6.0, 7.0, 7.5]),
    preset(
        "Soft Rock",
        [2.5, 2.5, 1.5, 0.0, -2.5, -3.5, -2.0, 0.0, 1.5, 5.5],
    ),
    preset(
        "Techno",
        [5.0, 3.5, 0.0, -3.5, -3.0, 0.0, 5.0, 6.0, 6.0, 5.5],
    ),
    preset(
        "Bass Booster",
        [6.0, 5.0, 4.0, 2.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    preset(
        "Bass Reducer",
        [-6.0, -5.0, -4.0, -2.5, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    preset(
        "Treble Booster",
        [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.5, 4.0, 5.0, 6.0],
    ),
    preset(
        "Vocal Booster",
        [-2.0, -3.0, -3.0, 1.0, 3.5, 3.5, 3.0, 1.5, 0.0, -1.5],
    ),
    preset(
        "Small Speakers",
        [5.5, 4.5, 3.5, 2.5, 1.0, 0.0, -1.0, -2.0, -2.5, -3.0],
    ),
    preset(
        "Spoken Word",
        [-3.5, -1.0, 0.0, 1.0, 3.5, 4.5, 4.5, 3.5, 2.0, 0.0],
    ),
    preset(
        "Loudness",
        [5.0, 3.5, 1.0, 0.0, -1.0, 0.0, 1.0, 3.0, 4.0, 4.0],
    ),
    preset(
        "Night Listening",
        [-3.0, -2.0, -1.0, 0.0, 1.0, 1.5, 1.0, -1.0, -2.0, -3.0],
    ),
];

const fn preset(name: &'static str, gains: [f64; 10]) -> Preset {
    Preset { name, gains }
}

/// The preset these gains are, if any.
pub fn preset_named(gains: &[f64; 10]) -> Option<&'static str> {
    PRESETS.iter().find(|p| &p.gains == gains).map(|p| p.name)
}

/// One biquad's coefficients, normalised (a0 = 1).
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn peaking(freq: f64, gain_db: f64, rate: f64) -> Biquad {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = std::f64::consts::TAU * freq / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * Q);
        let a0 = 1.0 + alpha / a;
        Biquad {
            b0: (1.0 + alpha * a) / a0,
            b1: -2.0 * cos / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha / a) / a0,
        }
    }

    /// The filter's gain at `freq`, as a factor.
    fn magnitude(&self, freq: f64, rate: f64) -> f64 {
        let w = std::f64::consts::TAU * freq / rate;
        // H(e^jw) = (b0 + b1 e^-jw + b2 e^-2jw) / (1 + a1 e^-jw + a2 e^-2jw)
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let (nr, ni) = (
            self.b0 + self.b1 * c1 + self.b2 * c2,
            -self.b1 * s1 - self.b2 * s2,
        );
        let (dr, di) = (
            1.0 + self.a1 * c1 + self.a2 * c2,
            -self.a1 * s1 - self.a2 * s2,
        );
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }

    /// Transposed direct form II, one sample.
    #[inline]
    fn run(&self, z: &mut [f64; 2], x: f64) -> f64 {
        let y = self.b0 * x + z[0];
        z[0] = self.b1 * x - self.a1 * y + z[1];
        z[1] = self.b2 * x - self.a2 * y;
        y
    }
}

/// The highest gain of the filters together, in dB, looked for across the
/// audible range (log-spaced, plus every band centre).
fn peak_db(filters: &[Biquad], rate: f64) -> f64 {
    (0..=240)
        .map(|i| 20.0 * 1_000f64.powf(f64::from(i) / 240.0))
        .chain(BANDS)
        .filter(|f| *f < rate / 2.0)
        .map(|f| {
            let gain: f64 = filters.iter().map(|b| b.magnitude(f, rate)).product();
            20.0 * gain.log10()
        })
        .fold(f64::MIN, f64::max)
}

/// The equalizer for one interleaved stream.
pub struct Equalizer {
    rate: f64,
    channels: usize,
    curve: Option<Curve>,
    input_gain: f64,
    /// Only the bands that do something.
    filters: Vec<Biquad>,
    /// Filter memory, per channel and filter.
    state: Vec<[f64; 2]>,
}

impl Equalizer {
    pub fn new(rate: u32, channels: usize) -> Equalizer {
        Equalizer {
            rate: f64::from(rate),
            channels: channels.max(1),
            curve: None,
            input_gain: 1.0,
            filters: Vec::new(),
            state: Vec::new(),
        }
    }

    /// The curve to apply; `None` (or a flat curve) leaves samples alone.
    /// Setting the curve it already has does nothing.
    pub fn set(&mut self, curve: Option<Curve>) {
        let curve = curve.filter(|c| !c.is_flat());
        if curve == self.curve {
            return;
        }
        self.curve = curve;
        self.filters = curve
            .map(|c| {
                BANDS
                    .iter()
                    .zip(c.gains)
                    .filter(|(_, gain)| *gain != 0.0)
                    .map(|(freq, gain)| Biquad::peaking(*freq, gain, self.rate))
                    .collect()
            })
            .unwrap_or_default();
        self.input_gain = curve.map_or(1.0, |c| {
            let headroom = peak_db(&self.filters, self.rate).max(0.0);
            10f64.powf((c.preamp - headroom) / 20.0)
        });
        self.state = vec![[0.0; 2]; self.channels * self.filters.len()];
    }

    /// Filters interleaved samples in place.
    pub fn process(&mut self, samples: &mut [f64]) {
        if self.curve.is_none() {
            return;
        }
        let n = self.filters.len();
        for frame in samples.chunks_exact_mut(self.channels) {
            for (channel, sample) in frame.iter_mut().enumerate() {
                let state = &mut self.state[channel * n..(channel + 1) * n];
                let mut x = *sample * self.input_gain;
                for (filter, z) in self.filters.iter().zip(state.iter_mut()) {
                    x = filter.run(z, x);
                }
                *sample = x;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// Plays a stereo sine through `eq` and returns its level change in dB,
    /// measured after the filters have settled.
    fn response(eq: &mut Equalizer, freq: f64) -> f64 {
        let frames = RATE as usize / 2;
        let mut samples: Vec<f64> = (0..frames)
            .flat_map(|i| {
                let s = 0.5 * (std::f64::consts::TAU * freq * i as f64 / f64::from(RATE)).sin();
                [s, s]
            })
            .collect();
        let before = rms(&samples[samples.len() / 2..]);
        eq.process(&mut samples);
        20.0 * (rms(&samples[samples.len() / 2..]) / before).log10()
    }

    fn rms(samples: &[f64]) -> f64 {
        (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt()
    }

    fn curve(gains: [f64; 10]) -> Option<Curve> {
        Some(Curve { preamp: 0.0, gains })
    }

    #[test]
    fn off_or_flat_leaves_the_samples_exactly_alone() {
        let original = vec![0.1, -0.2, 0.3, -0.4];
        for setting in [None, curve([0.0; 10])] {
            let mut eq = Equalizer::new(RATE, 2);
            eq.set(setting);
            let mut samples = original.clone();
            eq.process(&mut samples);
            assert_eq!(samples, original);
        }
    }

    #[test]
    fn a_boosted_band_stands_out_by_its_gain() {
        let mut gains = [0.0; 10];
        gains[4] = 6.0; // 1 kHz
        let mut eq = Equalizer::new(RATE, 2);
        eq.set(curve(gains));
        let at_band = response(&mut eq, 1_000.0);
        let mut eq = Equalizer::new(RATE, 2);
        eq.set(curve(gains));
        let far_away = response(&mut eq, 60.0);
        assert!(
            (at_band - far_away - 6.0).abs() < 0.5,
            "{at_band} vs {far_away}"
        );
    }

    #[test]
    fn a_boost_never_lifts_the_level_so_nothing_clips() {
        for freq in [60.0, 170.0, 1_000.0, 12_000.0, 14_000.0] {
            let mut e = Equalizer::new(RATE, 2);
            e.set(curve([MAX_GAIN_DB; 10]));
            assert!(response(&mut e, freq) < 0.5, "{freq} Hz");
        }
    }

    #[test]
    fn a_cut_takes_its_band_down() {
        let mut gains = [0.0; 10];
        gains[0] = -9.0; // 60 Hz
        let mut eq = Equalizer::new(RATE, 2);
        eq.set(curve(gains));
        let cut = response(&mut eq, 60.0);
        assert!((cut + 9.0).abs() < 1.0, "{cut}");
    }

    #[test]
    fn the_preamp_moves_everything() {
        let mut gains = [0.0; 10];
        gains[9] = -3.0; // keeps the curve from being flat; far from 1 kHz
        let mut eq = Equalizer::new(RATE, 2);
        eq.set(Some(Curve {
            preamp: -6.0,
            gains,
        }));
        assert!((response(&mut eq, 1_000.0) + 6.0).abs() < 0.3);
    }

    #[test]
    fn channels_do_not_bleed_into_each_other() {
        let mut eq = Equalizer::new(RATE, 2);
        eq.set(curve([6.0; 10]));
        let mut samples: Vec<f64> = (0..2_000)
            .flat_map(|i| [((i as f64) * 0.05).sin(), 0.0])
            .collect();
        eq.process(&mut samples);
        assert!(samples.iter().skip(1).step_by(2).all(|s| *s == 0.0));
        assert!(samples.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn presets_start_flat_have_unique_names_and_stay_in_range() {
        assert_eq!(PRESETS[0].name, "Flat");
        let mut names: Vec<_> = PRESETS.iter().map(|p| p.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), PRESETS.len());
        assert!(
            PRESETS
                .iter()
                .flat_map(|p| p.gains)
                .all(|g| g.abs() <= MAX_GAIN_DB)
        );
    }

    #[test]
    fn gains_are_recognised_as_their_preset() {
        assert_eq!(preset_named(&PRESETS[13].gains), Some("Rock"));
        assert_eq!(preset_named(&[1.0; 10]), None);
    }
}
