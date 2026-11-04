//! Time as plain milliseconds, passed in by the caller.
//!
//! The core never reads a clock. The host turns its monotonic clock into
//! [`Millis`] and hands it to every call, which keeps the rules deterministic
//! and lets tests move time forward by hand.

use std::ops::{Add, Sub};

/// A point on the host's monotonic clock, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Millis(pub u64);

impl Millis {
    pub const ZERO: Millis = Millis(0);

    pub fn from_secs(secs: u64) -> Millis {
        Millis(secs * 1000)
    }

    /// Whole seconds from `self` until `later`, rounded up; zero if `later` has passed.
    pub fn secs_until(self, later: Millis) -> u64 {
        later.0.saturating_sub(self.0).div_ceil(1000)
    }
}

impl Add<u64> for Millis {
    type Output = Millis;
    fn add(self, ms: u64) -> Millis {
        Millis(self.0 + ms)
    }
}

impl Sub for Millis {
    type Output = u64;
    /// Milliseconds between two points; zero if `rhs` is later.
    fn sub(self, rhs: Millis) -> u64 {
        self.0.saturating_sub(rhs.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_until_rounds_up_and_never_goes_negative() {
        assert_eq!(Millis(1_000).secs_until(Millis(1_001)), 1);
        assert_eq!(Millis(1_000).secs_until(Millis(6_000)), 5);
        assert_eq!(Millis(6_000).secs_until(Millis(1_000)), 0);
    }

    #[test]
    fn subtraction_saturates() {
        assert_eq!(Millis(500) - Millis(200), 300);
        assert_eq!(Millis(200) - Millis(500), 0);
    }
}
