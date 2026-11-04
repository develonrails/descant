//! The stream Spotify sends: the three Ogg Vorbis bitrates an account can
//! ask for. Spotify's own apps name them Normal, High and Very high, and a
//! Premium account is what unlocks the last one.
//!
//! The bitrate is fixed when the player is built, so changing it means
//! building a new one — the controller does that the way it reconnects.

/// How much of the stream to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// 96 kbps: for a slow or metered connection.
    Normal,
    /// 160 kbps, what Spotify gives an account that states no preference.
    #[default]
    High,
    /// 320 kbps, Premium only.
    VeryHigh,
}

impl Quality {
    /// The bitrate in kbps, as Spotify counts it.
    pub fn kbps(self) -> u16 {
        match self {
            Quality::Normal => 96,
            Quality::High => 160,
            Quality::VeryHigh => 320,
        }
    }

    /// The name this is stored under in GSettings.
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Normal => "normal",
            Quality::High => "high",
            Quality::VeryHigh => "very-high",
        }
    }

    /// What a stored name means. Anything else is a setting written by a
    /// newer version or by hand, which is no reason to refuse to play: take
    /// the one Spotify itself falls back to.
    pub fn from_name(name: &str) -> Quality {
        match name {
            "normal" => Quality::Normal,
            "very-high" => Quality::VeryHigh,
            _ => Quality::High,
        }
    }

    /// In the order the preferences list them, quietest first.
    pub const ALL: [Quality; 3] = [Quality::Normal, Quality::High, Quality::VeryHigh];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_quality_survives_being_stored() {
        for quality in Quality::ALL {
            assert_eq!(Quality::from_name(quality.as_str()), quality);
        }
    }

    #[test]
    fn an_unknown_setting_plays_at_the_usual_rate() {
        assert_eq!(Quality::from_name("lossless"), Quality::High);
        assert_eq!(Quality::from_name(""), Quality::High);
        assert_eq!(Quality::default(), Quality::High);
    }

    #[test]
    fn the_rates_are_the_ones_spotify_sends() {
        assert_eq!(Quality::Normal.kbps(), 96);
        assert_eq!(Quality::High.kbps(), 160);
        assert_eq!(Quality::VeryHigh.kbps(), 320);
    }
}
