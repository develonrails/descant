//! Spotify links and URIs, as people paste them.
//!
//! Accepts `spotify:track:ID` URIs and `open.spotify.com` links, including the
//! localised `/intl-xx/` form and the `?si=` share suffix, and turns them into
//! the canonical `spotify:kind:id` URI that librespot loads.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Track,
    Album,
    Playlist,
    Artist,
    /// A podcast.
    Show,
    /// One episode of a podcast.
    Episode,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Track => "track",
            Kind::Album => "album",
            Kind::Playlist => "playlist",
            Kind::Artist => "artist",
            Kind::Show => "show",
            Kind::Episode => "episode",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        match s {
            "track" => Some(Kind::Track),
            "album" => Some(Kind::Album),
            "playlist" => Some(Kind::Playlist),
            "artist" => Some(Kind::Artist),
            "show" => Some(Kind::Show),
            "episode" => Some(Kind::Episode),
            _ => None,
        }
    }
}

/// A playable Spotify item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyRef {
    pub kind: Kind,
    /// The 22-character base62 id.
    pub id: String,
}

impl SpotifyRef {
    /// Parses a pasted link or URI. Surrounding whitespace is ignored.
    pub fn parse(input: &str) -> Option<SpotifyRef> {
        let input = input.trim();
        let (kind, id) = if let Some(rest) = input.strip_prefix("spotify:") {
            rest.split_once(':')?
        } else {
            let path = input
                .strip_prefix("https://")
                .or_else(|| input.strip_prefix("http://"))
                .unwrap_or(input)
                .strip_prefix("open.spotify.com/")?;
            let path = path.split(['?', '#']).next()?;
            let mut segments = path.split('/').filter(|s| !s.is_empty());
            let mut kind = segments.next()?;
            if kind.starts_with("intl-") {
                kind = segments.next()?;
            }
            (kind, segments.next()?)
        };
        let kind = Kind::parse(kind)?;
        is_base62_id(id).then(|| SpotifyRef {
            kind,
            id: id.to_string(),
        })
    }

    /// Whether this is a context (something with tracks to play through).
    pub fn is_context(&self) -> bool {
        !matches!(self.kind, Kind::Track | Kind::Episode)
    }
}

fn is_base62_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

impl fmt::Display for SpotifyRef {
    /// The canonical URI, e.g. `spotify:track:3n3Ppam7vgaVa1iaRUc9Lp`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "spotify:{}:{}", self.kind.as_str(), self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "3n3Ppam7vgaVa1iaRUc9Lp";

    fn parsed(s: &str) -> Option<String> {
        SpotifyRef::parse(s).map(|r| r.to_string())
    }

    #[test]
    fn uris() {
        assert_eq!(
            parsed(&format!("spotify:track:{ID}")),
            Some(format!("spotify:track:{ID}"))
        );
        assert_eq!(
            parsed(&format!("spotify:album:{ID}")),
            Some(format!("spotify:album:{ID}"))
        );
    }

    #[test]
    fn share_links_with_tracking_suffix_and_locale() {
        let want = Some(format!("spotify:playlist:{ID}"));
        assert_eq!(
            parsed(&format!("https://open.spotify.com/playlist/{ID}")),
            want
        );
        assert_eq!(
            parsed(&format!("https://open.spotify.com/playlist/{ID}?si=abc123")),
            want
        );
        assert_eq!(
            parsed(&format!("open.spotify.com/intl-nl/playlist/{ID}")),
            want
        );
        assert_eq!(
            parsed(&format!("  https://open.spotify.com/playlist/{ID}/ \n")),
            want
        );
    }

    #[test]
    fn rejects_what_is_not_a_playable_item() {
        assert_eq!(parsed(""), None);
        assert_eq!(
            parsed("https://example.com/track/3n3Ppam7vgaVa1iaRUc9Lp"),
            None
        );
        assert_eq!(parsed("spotify:track:tooshort"), None);
        assert_eq!(parsed("spotify:track:3n3Ppam7vgaVa1iaRUc9L!"), None);
        assert_eq!(parsed("https://open.spotify.com/user/someone"), None);
    }

    #[test]
    fn shows_and_episodes_are_items_too() {
        assert_eq!(
            parsed(&format!("https://open.spotify.com/show/{ID}?si=x")),
            Some(format!("spotify:show:{ID}"))
        );
        assert_eq!(
            parsed(&format!("spotify:episode:{ID}")),
            Some(format!("spotify:episode:{ID}"))
        );
    }

    #[test]
    fn an_episode_plays_on_its_own_and_a_show_is_a_context() {
        assert!(
            !SpotifyRef::parse(&format!("spotify:episode:{ID}"))
                .unwrap()
                .is_context()
        );
        assert!(
            SpotifyRef::parse(&format!("spotify:show:{ID}"))
                .unwrap()
                .is_context()
        );
    }

    #[test]
    fn contexts_are_everything_but_a_single_track() {
        assert!(
            !SpotifyRef::parse(&format!("spotify:track:{ID}"))
                .unwrap()
                .is_context()
        );
        assert!(
            SpotifyRef::parse(&format!("spotify:album:{ID}"))
                .unwrap()
                .is_context()
        );
        assert!(
            SpotifyRef::parse(&format!("spotify:artist:{ID}"))
                .unwrap()
                .is_context()
        );
    }
}
