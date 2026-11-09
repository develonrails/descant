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
    /// Cover art, smallest first.
    pub covers: Vec<Cover>,
}

/// A picture at one size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    source: Source,
    pub width: u32,
}

/// Where a cover is. Nearly always a file on Spotify's image CDN, kept as
/// its 20-byte id rather than as a 64-byte address: every track has a few
/// sizes, and a long playlist thousands of tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Cdn([u8; 20]),
    /// Anything else (a mosaic, say), as given.
    Url(Box<str>),
}

/// Spotify's image CDN, which serves a cover by its file id in hex.
pub const IMAGE_CDN: &str = "https://i.scdn.co/image/";

impl Cover {
    /// The cover at `url`.
    pub fn new(url: &str, width: u32) -> Cover {
        let source = match url.strip_prefix(IMAGE_CDN).and_then(file_id) {
            Some(id) => Source::Cdn(id),
            None => Source::Url(url.into()),
        };
        Cover { source, width }
    }

    /// The CDN file with id `file`.
    pub fn file(file: &[u8], width: u32) -> Cover {
        let source = match <[u8; 20]>::try_from(file) {
            Ok(id) => Source::Cdn(id),
            Err(_) => Source::Url(cdn_url(file).into()),
        };
        Cover { source, width }
    }

    /// Where to fetch it.
    pub fn url(&self) -> String {
        match &self.source {
            Source::Cdn(id) => cdn_url(id),
            Source::Url(url) => url.to_string(),
        }
    }
}

/// A file's address on the CDN: its id in lower-case hex.
fn cdn_url(file: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut url = String::with_capacity(IMAGE_CDN.len() + 2 * file.len());
    url.push_str(IMAGE_CDN);
    for b in file {
        url.push(HEX[usize::from(b >> 4)] as char);
        url.push(HEX[usize::from(b & 0xf)] as char);
    }
    url
}

/// A file id as the CDN spells it (40 lower-case hex digits), so that the
/// address made from it again is the same.
fn file_id(hex: &str) -> Option<[u8; 20]> {
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let hex = hex.as_bytes();
    if hex.len() != 40 {
        return None;
    }
    let mut id = [0u8; 20];
    for (byte, pair) in id.iter_mut().zip(hex.chunks(2)) {
        *byte = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(id)
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
                Cover::new("s", 64),
                Cover::new("m", 300),
                Cover::new("l", 640),
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
        assert_eq!(info().cover_for(48).unwrap().url(), "s");
        assert_eq!(info().cover_for(64).unwrap().url(), "s");
        assert_eq!(info().cover_for(65).unwrap().url(), "s");
        assert_eq!(info().cover_for(68).unwrap().url(), "s");
        assert_eq!(info().cover_for(69).unwrap().url(), "m");
        assert_eq!(info().cover_for(300).unwrap().url(), "m");
        // a 160 px card at 200 %: the 300 px file, not the 640 px one
        assert_eq!(info().cover_for(320).unwrap().url(), "m");
        assert_eq!(info().cover_for(321).unwrap().url(), "l");
        assert_eq!(info().cover_for(2000).unwrap().url(), "l");
        assert_eq!(TrackInfo::default().cover_for(64), None);
    }

    const ALBUM: &str = "https://i.scdn.co/image/ab67616d0000b273c8b444df094279e70d0ed856";

    #[test]
    fn a_cdn_cover_is_kept_by_its_id_and_gives_back_its_address() {
        let cover = Cover::new(ALBUM, 640);
        assert!(matches!(cover.source, Source::Cdn(_)));
        assert_eq!(cover.url(), ALBUM);
        let id = file_id(&ALBUM[IMAGE_CDN.len()..]).unwrap();
        assert_eq!(Cover::file(&id, 640), cover, "the same file, however named");
    }

    #[test]
    fn other_addresses_are_kept_as_they_are() {
        for url in [
            "https://mosaic.scdn.co/640/ab67616d0000b273",
            // Upper-case hex would come back lower-case: kept as given.
            "https://i.scdn.co/image/AB67616D0000B273C8B444DF094279E70D0ED856",
            "https://i.scdn.co/image/ab01ff",
            "https://i.scdn.co/image/ab67616d0000b273c8b444df094279e70d0ed85g",
        ] {
            let cover = Cover::new(url, 300);
            assert!(matches!(cover.source, Source::Url(_)), "{url}");
            assert_eq!(cover.url(), url);
        }
        // A file id of another length still makes an address.
        assert_eq!(
            Cover::file(&[0xab, 0x01, 0xff], 64).url(),
            "https://i.scdn.co/image/ab01ff"
        );
    }
}
