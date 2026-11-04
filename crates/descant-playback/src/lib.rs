//! Descant's playback engine.
//!
//! librespot plays; this crate keeps it honest. [`sink::GuardedSink`] stops a
//! failing audio output from taking the whole app down,
//! [`equalizer::EqualizedSink`] applies the equalizer, [`tap`] hears the
//! audio-key refusals librespot only logs, and [`engine::LibrespotEngine`]
//! drives the player through Spirc and reports what happens as
//! [`descant_core::supervisor::Input`]s.

pub mod controller;
pub mod engine;
pub mod equalizer;
pub mod sink;
pub mod tap;
