//! The audio output: PulseAudio (PipeWire's pulse server), fed in tenths of
//! a second.
//!
//! librespot's own PulseAudio backend opens its stream with the server's
//! default buffering and writes every decoded packet (1024 frames, ~23 ms) as
//! it comes. Under pipewire-pulse that means a 20 ms request size, and the
//! client's mainloop thread waking around 115 times a second for as long as
//! music plays. It also hands PipeWire 16-bit samples, made by dithering every
//! sample into a freshly allocated buffer, which PipeWire turns straight back
//! into floats.
//!
//! [`PulseSink`] asks for explicit buffering instead — [`TARGET_MS`] of audio
//! queued, topped up [`REQUEST_MS`] at a time — and answers each request with
//! one write of exactly that much: 32-bit float samples, converted into a
//! buffer it keeps. Floats need no dither, so nothing random is drawn per
//! sample. The mainloop thread wakes around 30 times a second.
//!
//! ## How late things are heard
//!
//! libpulse's simple API asks the server to adjust latency, so the target is
//! the whole delay from write to speaker: pipewire-pulse splits the 400 ms
//! into 300 ms of stream buffer and 100 ms of node latency. On top of that,
//! up to one batch (100 ms) can wait here for the packet that completes it.
//!
//! Volume is applied by librespot before the sink, and seeking does not touch
//! the sink, so a volume change or a seek is heard once that queue has
//! played: 400–500 ms, against about 250 ms with librespot's backend under
//! pipewire-pulse's defaults.
//!
//! Pausing stops the sink. Like librespot's backend, [`PulseSink::stop`]
//! writes out what is waiting, lets the server play the queue to the end
//! (drains) and closes the stream, so nothing is lost and a resumed track
//! carries on where it was heard to stop. A pause is therefore heard after
//! the queue, too: ~425 ms measured, against ~255 ms. A shorter target would
//! make it quicker but costs wakeups: 300 ms measured twice the rate of 400.

use std::env;

use libpulse_binding::{def::BufferAttr, sample, stream::Direction};
use libpulse_simple_binding::Simple;
use librespot_playback::{
    NUM_CHANNELS, SAMPLE_RATE,
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};

/// How much audio is queued between a write and the speaker, in milliseconds.
pub const TARGET_MS: u32 = 400;
/// How much the server asks for at a time, and how much is written at once.
pub const REQUEST_MS: u32 = 100;

/// One frame: a 32-bit float per channel.
const FRAME_BYTES: u32 = NUM_CHANNELS as u32 * size_of::<f32>() as u32;

/// The bytes `ms` of audio take, a whole number of frames.
fn bytes_for(ms: u32) -> u32 {
    SAMPLE_RATE * ms / 1000 * FRAME_BYTES
}

/// The buffering asked of the server.
fn buffer_attr() -> BufferAttr {
    BufferAttr {
        // the server's maximum, as recommended
        maxlength: u32::MAX,
        tlength: bytes_for(TARGET_MS),
        // the server's default: start once most of the target is queued
        prebuf: u32::MAX,
        minreq: bytes_for(REQUEST_MS),
        // playback only
        fragsize: u32::MAX,
    }
}

/// Samples converted to the stream's format, held until a whole batch can be
/// written at once.
struct Batch {
    bytes: Vec<u8>,
    size: usize,
}

impl Batch {
    fn new(size: usize) -> Batch {
        Batch {
            // a batch, plus the packet that overfills it
            bytes: Vec::with_capacity(size * 2),
            size,
        }
    }

    /// Adds `samples` and hands every whole batch now held to `write`, in one
    /// call; the rest waits for the next packet.
    fn push<E>(
        &mut self,
        samples: &[f64],
        write: impl FnOnce(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        self.bytes.reserve(samples.len() * size_of::<f32>());
        for &sample in samples {
            self.bytes.extend_from_slice(&(sample as f32).to_ne_bytes());
        }
        let whole = self.bytes.len() / self.size * self.size;
        if whole == 0 {
            return Ok(());
        }
        let written = write(&self.bytes[..whole]);
        self.bytes.drain(..whole);
        written
    }

    /// Hands whatever is held to `write`, whole batch or not.
    fn finish<E>(&mut self, write: impl FnOnce(&[u8]) -> Result<(), E>) -> Result<(), E> {
        if self.bytes.is_empty() {
            return Ok(());
        }
        let written = write(&self.bytes);
        self.bytes.clear();
        written
    }
}

pub struct PulseSink {
    device: Option<String>,
    app_name: String,
    stream_name: String,
    stream: Option<Simple>,
    batch: Batch,
}

impl PulseSink {
    /// A sink for `device`, or the server's default sink (which `PULSE_SINK`
    /// can set) when `None`. Nothing connects until [`Sink::start`].
    pub fn new(device: Option<String>) -> PulseSink {
        // librespot's backend names itself from these, so honour them too
        let app_name =
            env::var("PULSE_PROP_application.name").unwrap_or_else(|_| "Descant".to_string());
        let stream_name =
            env::var("PULSE_PROP_stream.description").unwrap_or_else(|_| "Music".to_string());
        PulseSink {
            device,
            app_name,
            stream_name,
            stream: None,
            batch: Batch::new(bytes_for(REQUEST_MS) as usize),
        }
    }
}

impl Sink for PulseSink {
    fn start(&mut self) -> SinkResult<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        let spec = sample::Spec {
            format: sample::Format::FLOAT32NE,
            channels: NUM_CHANNELS,
            rate: SAMPLE_RATE,
        };
        let stream = Simple::new(
            None, // the default server
            &self.app_name,
            Direction::Playback,
            self.device.as_deref(),
            &self.stream_name,
            &spec,
            None, // the default channel map
            Some(&buffer_attr()),
        )
        .map_err(|e| SinkError::ConnectionRefused(format!("PulseAudio: {e}")))?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Plays out what is queued, then closes the stream.
    fn stop(&mut self) -> SinkResult<()> {
        let Some(stream) = self.stream.take() else {
            return Ok(());
        };
        self.batch
            .finish(|bytes| stream.write(bytes))
            .map_err(|e| SinkError::OnWrite(format!("PulseAudio: {e}")))?;
        stream
            .drain()
            .map_err(|e| SinkError::StateChange(format!("PulseAudio: failed to drain, {e}")))
    }

    fn write(&mut self, packet: AudioPacket, _: &mut Converter) -> SinkResult<()> {
        let Some(stream) = self.stream.as_ref() else {
            return Err(SinkError::NotConnected(
                "PulseAudio: the stream is not open".into(),
            ));
        };
        let AudioPacket::Samples(samples) = packet else {
            return Err(SinkError::InvalidParams(
                "PulseAudio: cannot play undecoded audio".into(),
            ));
        };
        self.batch
            .push(&samples, |bytes| stream.write(bytes))
            .map_err(|e| SinkError::OnWrite(format!("PulseAudio: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a batch handed out, as one entry per write.
    fn pushed(batch: &mut Batch, samples: &[f64]) -> Vec<Vec<u8>> {
        let mut writes = Vec::new();
        batch
            .push(samples, |b| {
                writes.push(b.to_vec());
                Ok::<_, ()>(())
            })
            .unwrap();
        writes
    }

    fn floats(bytes: &[u8]) -> Vec<f32> {
        let (whole, rest) = bytes.as_chunks::<4>();
        assert!(rest.is_empty());
        whole.iter().map(|b| f32::from_ne_bytes(*b)).collect()
    }

    #[test]
    fn the_buffer_is_a_whole_number_of_frames_in_tenths_of_a_second() {
        let attr = buffer_attr();
        assert_eq!(attr.minreq, 4410 * 8);
        assert_eq!(attr.tlength, 4 * attr.minreq);
        assert_eq!(attr.tlength % FRAME_BYTES, 0);
        assert_eq!(attr.prebuf, u32::MAX, "the server's default");
    }

    #[test]
    fn samples_become_native_floats() {
        let mut batch = Batch::new(16);
        let writes = pushed(&mut batch, &[0.5, -0.25, 1.0, 0.0]);
        assert_eq!(writes.len(), 1);
        assert_eq!(floats(&writes[0]), vec![0.5, -0.25, 1.0, 0.0]);
    }

    #[test]
    fn nothing_is_written_until_a_whole_batch_is_held() {
        let mut batch = Batch::new(16);
        assert!(pushed(&mut batch, &[0.1, 0.2]).is_empty());
        assert!(pushed(&mut batch, &[0.3]).is_empty());
        let writes = pushed(&mut batch, &[0.4, 0.5]);
        assert_eq!(writes.len(), 1);
        assert_eq!(floats(&writes[0]), vec![0.1, 0.2, 0.3, 0.4]);
        // the fifth sample waits for the next batch
        let mut rest = Vec::new();
        batch
            .finish(|b| {
                rest = floats(b);
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(rest, vec![0.5]);
    }

    #[test]
    fn a_large_packet_is_written_as_whole_batches_in_one_go() {
        let mut batch = Batch::new(8);
        let samples: Vec<f64> = (0..5).map(f64::from).collect();
        let writes = pushed(&mut batch, &samples);
        assert_eq!(writes.len(), 1, "one write");
        assert_eq!(floats(&writes[0]), vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(batch.bytes.len(), 4, "one sample left over");
    }

    #[test]
    fn finishing_an_empty_batch_writes_nothing() {
        let mut batch = Batch::new(8);
        let mut called = false;
        batch
            .finish(|_| {
                called = true;
                Ok::<_, ()>(())
            })
            .unwrap();
        assert!(!called);
    }

    #[test]
    fn a_failed_write_still_lets_go_of_the_batch() {
        let mut batch = Batch::new(8);
        assert!(batch.push(&[0.1, 0.2, 0.3], |_| Err(())).is_err());
        assert_eq!(batch.bytes.len(), 4, "only the leftover is kept");
    }

    /// Plays a sine for a while and reports how often libpulse's thread woke,
    /// and how long stopping (draining) took. Send it to a null sink:
    ///
    /// ```text
    /// pactl load-module module-null-sink sink_name=descant_test
    /// PULSE_SINK=descant_test cargo test -p descant-playback -- --ignored --nocapture plays_a_sine
    /// pactl unload-module <index>
    /// ```
    #[test]
    #[ignore = "plays through the PulseAudio server; point PULSE_SINK at a null sink"]
    fn plays_a_sine_through_the_server() {
        use std::time::{Duration, Instant};

        let mut sink = PulseSink::new(None);
        let mut converter = Converter::new(None);
        let rate = f64::from(SAMPLE_RATE);
        let mut frame = 0u64;
        let mut play = |sink: &mut PulseSink, seconds: f64| {
            let until = frame + (seconds * rate) as u64;
            while frame < until {
                let samples: Vec<f64> = (frame..frame + 1024)
                    .flat_map(|f| {
                        let s = 0.5 * (std::f64::consts::TAU * 440.0 * f as f64 / rate).sin();
                        [s, s]
                    })
                    .collect();
                frame += 1024;
                sink.write(AudioPacket::Samples(samples), &mut converter)
                    .unwrap();
            }
        };

        sink.start().unwrap();
        play(&mut sink, 2.0);
        let (before, since) = (pulse_thread_switches(), Instant::now());
        play(&mut sink, 8.0);
        let switches = pulse_thread_switches() - before;
        let elapsed = since.elapsed();
        let stopping = Instant::now();
        sink.stop().unwrap();
        let drained = stopping.elapsed();

        let per_second = switches as f64 / elapsed.as_secs_f64();
        println!(
            "pulse thread: {switches} context switches in {:.2} s = {per_second:.1}/s; \
             stop (drain) took {} ms",
            elapsed.as_secs_f64(),
            drained.as_millis()
        );
        assert!(elapsed > Duration::from_secs(7), "written in real time");
    }

    /// Context switches so far of libpulse's mainloop threads in this process.
    fn pulse_thread_switches() -> u64 {
        let mut total = 0;
        for task in std::fs::read_dir("/proc/self/task").unwrap() {
            let path = task.unwrap().path();
            let comm = std::fs::read_to_string(path.join("comm")).unwrap_or_default();
            if comm.trim() != "threaded-ml" {
                continue;
            }
            let status = std::fs::read_to_string(path.join("status")).unwrap_or_default();
            total += status
                .lines()
                .filter(|l| l.contains("ctxt_switches"))
                .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
                .sum::<u64>();
        }
        total
    }
}
