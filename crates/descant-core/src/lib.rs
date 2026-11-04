//! Descant's core: the rules that decide what playback does, and the small
//! domain types they need. No I/O, no GTK, no librespot — everything here is
//! driven by explicit inputs (including the time) so it can be tested exactly.

pub mod equalizer;
pub mod key;
pub mod link;
pub mod podcast;
pub mod supervisor;
pub mod time;
pub mod track;
pub mod volume;
