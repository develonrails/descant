//! What the UI shows about the track that plays.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackInfo {
    pub uri: String,
    pub title: String,
    pub artists: Vec<String>,
    /// `spotify:artist:` URIs, in the order of `artists` (may be empty).
    pub artist_uris: Vec<String>,
    pub album: String,
    /// `spotify:album:` URI (may be empty).
    pub album_uri: String,
    pub duration_ms: u32,
    /// Cover art URLs, smallest first.
    pub covers: Vec<Cover>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    pub url: String,
    pub width: u32,
}

impl TrackInfo {
    /// "Artist, Other Artist".
    pub fn artist_line(&self) -> String {
        self.artists.join(", ")
    }

    /// The smallest cover at least `min_width` wide, else the largest there is.
    pub fn cover_for(&self, min_width: u32) -> Option<&Cover> {
        cover_at_least(&self.covers, min_width)
    }
}

/// The smallest of `covers` (smallest first) at least `min_width` wide, else
/// the largest there is. Fetching the right size is most of what keeps album
/// art cheap.
pub fn cover_at_least(covers: &[Cover], min_width: u32) -> Option<&Cover> {
    covers
        .iter()
        .find(|c| c.width >= min_width)
        .or_else(|| covers.last())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> TrackInfo {
        TrackInfo {
            artists: vec!["Radiohead".into(), "Someone".into()],
            covers: vec![
                Cover {
                    url: "s".into(),
                    width: 64,
                },
                Cover {
                    url: "m".into(),
                    width: 300,
                },
                Cover {
                    url: "l".into(),
                    width: 640,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn artists_are_joined() {
        assert_eq!(info().artist_line(), "Radiohead, Someone");
    }

    #[test]
    fn picks_the_smallest_cover_that_is_big_enough() {
        assert_eq!(info().cover_for(48).unwrap().url, "s");
        assert_eq!(info().cover_for(64).unwrap().url, "s");
        assert_eq!(info().cover_for(65).unwrap().url, "m");
        assert_eq!(info().cover_for(2000).unwrap().url, "l");
        assert_eq!(TrackInfo::default().cover_for(64), None);
    }
}
