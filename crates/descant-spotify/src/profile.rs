//! Who is signed in, for Preferences: a display name and a picture.

use librespot_core::{Error, session::Session};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    /// `None` when the account has no display name of its own (Spotify then
    /// shows the account id in its place).
    pub name: Option<String>,
    pub image_url: Option<String>,
}

/// The signed-in account's profile.
pub async fn me(session: &Session) -> Result<Profile, Error> {
    let username = session.username();
    let endpoint = format!(
        "/user-profile-view/v3/profile/{username}?playlist_limit=0&artist_limit=0&episode_limit=0"
    );
    let bytes = session
        .spclient()
        .request(&http::Method::GET, &endpoint, None, None)
        .await?;
    Ok(parse(
        &serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        &username,
    ))
}

pub(crate) fn parse(profile: &Value, username: &str) -> Profile {
    let text = |key: &str| {
        profile[key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    Profile {
        name: text("name").filter(|name| name != username),
        image_url: text("image_url"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_display_name_and_picture_are_read() {
        let profile = json!({"name": "Mitchell", "image_url": "https://i.scdn.co/image/x"});
        assert_eq!(
            parse(&profile, "1234567890"),
            Profile {
                name: Some("Mitchell".into()),
                image_url: Some("https://i.scdn.co/image/x".into()),
            }
        );
    }

    #[test]
    fn the_account_id_standing_in_for_a_name_is_no_name() {
        let profile = json!({"name": "1234567890", "uri": "spotify:user:1234567890"});
        assert_eq!(parse(&profile, "1234567890"), Profile::default());
        assert_eq!(parse(&Value::Null, "x"), Profile::default());
    }
}
