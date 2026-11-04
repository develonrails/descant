//! The equalizer in the audio path: a sink that filters samples on their way
//! to the real one.
//!
//! The UI sets the curve through an [`EqualizerSetting`] it shares with the
//! sink; the sink picks up a change on its next packet. With the equalizer
//! off, packets pass through untouched.

use std::sync::{Arc, Mutex};

use descant_core::equalizer::{Curve, Equalizer};
use librespot_playback::{
    NUM_CHANNELS, SAMPLE_RATE,
    audio_backend::{Sink, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};

/// The curve to play with; `None` is off. Shared between the UI and the sink.
pub type EqualizerSetting = Arc<Mutex<Option<Curve>>>;

pub struct EqualizedSink {
    inner: Box<dyn Sink>,
    setting: EqualizerSetting,
    equalizer: Equalizer,
}

impl EqualizedSink {
    pub fn new(inner: Box<dyn Sink>, setting: EqualizerSetting) -> EqualizedSink {
        EqualizedSink {
            inner,
            setting,
            equalizer: Equalizer::new(SAMPLE_RATE, usize::from(NUM_CHANNELS)),
        }
    }
}

impl Sink for EqualizedSink {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.inner.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let packet = match packet {
            AudioPacket::Samples(mut samples) => {
                let curve = self.setting.lock().map(|s| *s).unwrap_or(None);
                self.equalizer.set(curve);
                self.equalizer.process(&mut samples);
                AudioPacket::Samples(samples)
            }
            raw => raw,
        };
        self.inner.write(packet, converter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps the samples it was given.
    struct Capture(Arc<Mutex<Vec<f64>>>);

    impl Sink for Capture {
        fn write(&mut self, packet: AudioPacket, _: &mut Converter) -> SinkResult<()> {
            if let AudioPacket::Samples(s) = packet {
                self.0.lock().unwrap().extend(s);
            }
            Ok(())
        }
    }

    fn played(curve: Option<Curve>) -> (Vec<f64>, Vec<f64>) {
        let input: Vec<f64> = (0..512).map(|i| (i as f64 * 0.1).sin() * 0.5).collect();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let setting: EqualizerSetting = Arc::new(Mutex::new(curve));
        let mut sink = EqualizedSink::new(Box::new(Capture(captured.clone())), setting);
        sink.write(
            AudioPacket::Samples(input.clone()),
            &mut Converter::new(None),
        )
        .unwrap();
        let out = captured.lock().unwrap().clone();
        (input, out)
    }

    #[test]
    fn off_passes_samples_through_untouched() {
        let (input, out) = played(None);
        assert_eq!(input, out);
    }

    #[test]
    fn on_filters_the_samples() {
        let mut gains = [0.0; 10];
        gains[0] = 9.0;
        let (input, out) = played(Some(Curve { preamp: 0.0, gains }));
        assert_eq!(input.len(), out.len());
        assert_ne!(input, out);
    }
}
