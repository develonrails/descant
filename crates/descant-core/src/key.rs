//! Audio-key failures, and what they mean.
//!
//! Every track needs an AES key from Spotify before it can be decrypted. When
//! Spotify refuses, it answers with a two-byte code. Spike S1
//! (`spikes/s1-playback`) measured:
//!
//! * `00 02` after rapid skipping on a working account: a budget of roughly 24
//!   keys, refilling at about one per 30–35 s. Temporary.
//! * `00 01` is what accounts behind Spotify's PlayPlay DRM get on every
//!   track (librespot#1649). Permanent for librespot.
//!
//! librespot logs the refusal as `error audio key {:x} {:x}` and then, by
//! default, tries to play the track undecrypted — which is silence.

/// Why Spotify refused an audio key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// `00 02`: too many keys too fast. Wait and try the same track again.
    RateLimited,
    /// `00 01`: this account's audio is behind PlayPlay DRM.
    Blocked,
    /// Any other code, kept for the log.
    Other(u8, u8),
}

impl KeyError {
    pub fn from_code(first: u8, second: u8) -> KeyError {
        match (first, second) {
            (0, 2) => KeyError::RateLimited,
            (0, 1) => KeyError::Blocked,
            (a, b) => KeyError::Other(a, b),
        }
    }

    /// Reads librespot's `error audio key 0 2` log line.
    pub fn from_log_line(line: &str) -> Option<KeyError> {
        let rest = line.trim().strip_prefix("error audio key ")?;
        let mut parts = rest.split_whitespace();
        let first = u8::from_str_radix(parts.next()?, 16).ok()?;
        let second = u8::from_str_radix(parts.next()?, 16).ok()?;
        Some(KeyError::from_code(first, second))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_measured_in_spike_s1() {
        assert_eq!(KeyError::from_code(0, 2), KeyError::RateLimited);
        assert_eq!(KeyError::from_code(0, 1), KeyError::Blocked);
        assert_eq!(KeyError::from_code(1, 7), KeyError::Other(1, 7));
    }

    #[test]
    fn reads_librespots_log_line() {
        assert_eq!(
            KeyError::from_log_line("error audio key 0 2"),
            Some(KeyError::RateLimited)
        );
        assert_eq!(
            KeyError::from_log_line("error audio key 0 1"),
            Some(KeyError::Blocked)
        );
        // librespot formats the bytes as hex
        assert_eq!(
            KeyError::from_log_line("error audio key 0 1f"),
            Some(KeyError::Other(0, 0x1f))
        );
    }

    #[test]
    fn ignores_other_lines() {
        assert_eq!(
            KeyError::from_log_line("Unable to load key, continuing without decryption"),
            None
        );
        assert_eq!(KeyError::from_log_line("error audio key"), None);
        assert_eq!(KeyError::from_log_line("error audio key x y"), None);
    }
}
