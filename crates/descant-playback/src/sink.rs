//! An audio sink that fails without taking the app down.
//!
//! librespot's player pauses when its sink fails to start or write, but calls
//! `exit(1)` when it fails to *stop* — fine for a daemon under systemd, fatal
//! for a desktop app. [`GuardedSink`] wraps the real sink (PulseAudio), reports
//! every failure, never lets a stop failure through, and builds a fresh sink
//! on the next start so a restarted PipeWire is picked up.

use librespot_playback::{
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};

/// Builds the real sink. Called again after every failure.
pub type BuildSink = Box<dyn FnMut() -> Box<dyn Sink> + Send>;
/// Told about every failure, with the sink's error message.
pub type ReportFailure = Box<dyn Fn(String) + Send>;

pub struct GuardedSink {
    build: BuildSink,
    report: ReportFailure,
    inner: Option<Box<dyn Sink>>,
}

impl GuardedSink {
    pub fn new(build: BuildSink, report: ReportFailure) -> GuardedSink {
        GuardedSink {
            build,
            report,
            inner: None,
        }
    }

    /// Drops the failed sink so the next start builds a fresh one.
    fn failed(&mut self, error: &SinkError) {
        self.inner = None;
        (self.report)(error.to_string());
    }
}

impl Sink for GuardedSink {
    fn start(&mut self) -> SinkResult<()> {
        let inner = self.inner.get_or_insert_with(|| (self.build)());
        inner.start().inspect_err(|e| self.failed(e))
    }

    fn stop(&mut self) -> SinkResult<()> {
        if let Some(inner) = self.inner.as_mut()
            && let Err(e) = inner.stop()
        {
            self.failed(&e);
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let Some(inner) = self.inner.as_mut() else {
            return Err(SinkError::NotConnected(
                "the audio output is not open".into(),
            ));
        };
        inner
            .write(packet, converter)
            .inspect_err(|e| self.failed(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What the fake sinks should do, and what happened.
    #[derive(Default)]
    struct Script {
        fail_start: bool,
        fail_write: bool,
        fail_stop: bool,
        built: usize,
        calls: Vec<&'static str>,
        reports: Vec<String>,
    }

    struct FakeSink(Arc<Mutex<Script>>);

    impl Sink for FakeSink {
        fn start(&mut self) -> SinkResult<()> {
            let mut s = self.0.lock().unwrap();
            s.calls.push("start");
            if s.fail_start {
                Err(SinkError::ConnectionRefused("no server".into()))
            } else {
                Ok(())
            }
        }
        fn stop(&mut self) -> SinkResult<()> {
            let mut s = self.0.lock().unwrap();
            s.calls.push("stop");
            if s.fail_stop {
                Err(SinkError::StateChange("drain failed".into()))
            } else {
                Ok(())
            }
        }
        fn write(&mut self, _: AudioPacket, _: &mut Converter) -> SinkResult<()> {
            let mut s = self.0.lock().unwrap();
            s.calls.push("write");
            if s.fail_write {
                Err(SinkError::OnWrite("broken pipe".into()))
            } else {
                Ok(())
            }
        }
    }

    fn guarded(script: &Arc<Mutex<Script>>) -> GuardedSink {
        let for_build = script.clone();
        let for_report = script.clone();
        GuardedSink::new(
            Box::new(move || {
                for_build.lock().unwrap().built += 1;
                Box::new(FakeSink(for_build.clone()))
            }),
            Box::new(move |msg| for_report.lock().unwrap().reports.push(msg)),
        )
    }

    fn packet() -> AudioPacket {
        AudioPacket::Samples(vec![0.0; 8])
    }

    #[test]
    fn a_healthy_sink_is_built_once_and_passed_through() {
        let script = Arc::new(Mutex::new(Script::default()));
        let mut sink = guarded(&script);
        let mut conv = Converter::new(None);
        sink.start().unwrap();
        sink.write(packet(), &mut conv).unwrap();
        sink.stop().unwrap();
        sink.start().unwrap();
        let s = script.lock().unwrap();
        assert_eq!(s.built, 1);
        assert_eq!(s.calls, vec!["start", "write", "stop", "start"]);
        assert!(s.reports.is_empty());
    }

    #[test]
    fn a_failed_start_is_reported_and_the_next_start_builds_a_new_sink() {
        let script = Arc::new(Mutex::new(Script {
            fail_start: true,
            ..Default::default()
        }));
        let mut sink = guarded(&script);
        // returned, so librespot pauses instead of claiming to play
        assert!(sink.start().is_err());
        script.lock().unwrap().fail_start = false;
        sink.start().unwrap();
        let s = script.lock().unwrap();
        assert_eq!(s.built, 2);
        assert_eq!(s.reports.len(), 1);
        assert!(s.reports[0].contains("no server"));
    }

    #[test]
    fn a_failed_stop_is_reported_but_never_returned() {
        // returning it would make librespot exit(1)
        let script = Arc::new(Mutex::new(Script {
            fail_stop: true,
            ..Default::default()
        }));
        let mut sink = guarded(&script);
        sink.start().unwrap();
        assert!(sink.stop().is_ok());
        sink.start().unwrap();
        let s = script.lock().unwrap();
        assert_eq!(s.reports.len(), 1);
        assert_eq!(s.built, 2, "a sink that failed to stop is not reused");
    }

    #[test]
    fn a_failed_write_is_reported_and_returned() {
        let script = Arc::new(Mutex::new(Script {
            fail_write: true,
            ..Default::default()
        }));
        let mut sink = guarded(&script);
        let mut conv = Converter::new(None);
        sink.start().unwrap();
        assert!(sink.write(packet(), &mut conv).is_err());
        script.lock().unwrap().fail_write = false;
        sink.start().unwrap();
        sink.write(packet(), &mut conv).unwrap();
        let s = script.lock().unwrap();
        assert_eq!(s.reports.len(), 1);
        assert_eq!(s.built, 2);
    }

    #[test]
    fn writing_with_no_open_sink_is_an_error_not_a_panic() {
        let script = Arc::new(Mutex::new(Script {
            fail_start: true,
            ..Default::default()
        }));
        let mut sink = guarded(&script);
        let mut conv = Converter::new(None);
        let _ = sink.start();
        assert!(sink.write(packet(), &mut conv).is_err());
        assert!(sink.stop().is_ok());
    }
}
