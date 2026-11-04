//! Volume as people think of it (0–100 %) and as Spotify Connect carries it
//! (0–65535). librespot's soft mixer applies a logarithmic curve on top, so a
//! linear slider sounds even.

/// Connect's full scale.
pub const MAX: u16 = u16::MAX;

pub fn from_percent(percent: f64) -> u16 {
    (percent.clamp(0.0, 100.0) / 100.0 * MAX as f64).round() as u16
}

pub fn to_percent(volume: u16) -> f64 {
    volume as f64 / MAX as f64 * 100.0
}

/// The symbolic icon for a level.
pub fn icon(percent: f64) -> &'static str {
    match percent {
        p if p <= 0.0 => "audio-volume-muted-symbolic",
        p if p < 34.0 => "audio-volume-low-symbolic",
        p if p < 67.0 => "audio-volume-medium-symbolic",
        _ => "audio-volume-high-symbolic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_maps_onto_the_full_connect_range() {
        assert_eq!(from_percent(0.0), 0);
        assert_eq!(from_percent(100.0), MAX);
        assert_eq!(from_percent(50.0), 32_768);
        assert_eq!(from_percent(150.0), MAX, "clamped");
        assert_eq!(from_percent(-3.0), 0, "clamped");
    }

    #[test]
    fn and_back() {
        assert_eq!(to_percent(MAX), 100.0);
        assert_eq!(to_percent(0), 0.0);
        assert!((to_percent(from_percent(37.0)) - 37.0).abs() < 0.01);
    }

    #[test]
    fn icons_follow_the_level() {
        assert_eq!(icon(0.0), "audio-volume-muted-symbolic");
        assert_eq!(icon(20.0), "audio-volume-low-symbolic");
        assert_eq!(icon(50.0), "audio-volume-medium-symbolic");
        assert_eq!(icon(90.0), "audio-volume-high-symbolic");
    }
}
