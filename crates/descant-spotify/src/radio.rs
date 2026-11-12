//! Song radio: the playlist Spotify mixes from one track, as "Go to Song
//! Radio" opens it in the official client.

use librespot_core::{Error, session::Session, spotify_uri::SpotifyUri};
use serde_json::Value;

/// The radio playlist for `track` (a `spotify:track:` URI).
pub async fn for_track(session: &Session, track: &str) -> Result<String, Error> {
    let uri = SpotifyUri::from_uri(track)?;
    let bytes = session.spclient().get_radio_for_track(&uri).await?;
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    playlist(&value).ok_or_else(|| Error::not_found(format!("no radio for {track}")))
}

/// The first playlist in an `inspiredby-mix` answer's `mediaItems`.
fn playlist(answer: &Value) -> Option<String> {
    answer["mediaItems"]
        .as_array()?
        .iter()
        .filter_map(|item| item["uri"].as_str())
        .find(|uri| uri.starts_with("spotify:playlist:"))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_radio_is_the_first_playlist_in_the_answer() {
        let answer = json!({"mediaItems": [
            {"uri": "spotify:station:track:x"},
            {"uri": "spotify:playlist:37i9dQZF1E8radio"},
            {"uri": "spotify:playlist:other"}
        ]});
        assert_eq!(
            playlist(&answer).as_deref(),
            Some("spotify:playlist:37i9dQZF1E8radio")
        );
    }

    #[test]
    fn an_answer_without_a_playlist_has_no_radio() {
        assert_eq!(playlist(&json!({"mediaItems": []})), None);
        assert_eq!(playlist(&json!({"mediaItems": [{"name": "x"}]})), None);
        assert_eq!(playlist(&json!(null)), None);
    }
}
