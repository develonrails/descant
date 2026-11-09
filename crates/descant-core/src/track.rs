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

    /// The smallest cover wide enough for `min_width`, else the largest there
    /// is; see [`cover_at_least`].
    pub fn cover_for(&self, min_width: u32) -> Option<&Cover> {
        cover_at_least(&self.covers, min_width)
    }
}

/// The smallest of `covers` (smallest first) wide enough for `min_width`,
/// else the largest there is. Fetching the right size is most of what keeps
/// album art cheap.
///
/// "Wide enough" allows a sixteenth short: a 160 px card at 200 % fills 320
/// pixels, and the 300 px rendition drawn 7 % larger there looks the same as
/// the 640 px one cut down, for a quarter of the download and of the decode.
/// Further short than that, the larger rendition is taken and cut to size.
pub fn cover_at_least(covers: &[Cover], min_width: u32) -> Option<&Cover> {
    covers
        .iter()
        .find(|c| u64::from(c.width) * 16 >= u64::from(min_width) * 15)
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
        assert_eq!(info().cover_for(65).unwrap().url, "s");
        assert_eq!(info().cover_for(68).unwrap().url, "s");
        assert_eq!(info().cover_for(69).unwrap().url, "m");
        assert_eq!(info().cover_for(300).unwrap().url, "m");
        // a 160 px card at 200 %: the 300 px file, not the 640 px one
        assert_eq!(info().cover_for(320).unwrap().url, "m");
        assert_eq!(info().cover_for(321).unwrap().url, "l");
        assert_eq!(info().cover_for(2000).unwrap().url, "l");
        assert_eq!(TrackInfo::default().cover_for(64), None);
    }
}
