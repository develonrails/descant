//! Pairing with a phone: OAuth's device authorization flow.
//!
//! Spotify issues a short code and a link with that code in it
//! (`https://spotify.com/pair?code=ABCDEF`). Descant shows the link as a QR
//! code; scanning it on the phone opens Spotify's pairing page with the code
//! filled in, and confirming there logs Descant in. Nothing listens on a port,
//! and the desktop never sees a password.
//!
//! The access token that comes back is traded once for reusable credentials
//! (librespot's stored-credentials blob), which is what the keyring keeps.

use std::time::Duration;

use librespot_core::{
    Error, authentication::Credentials, cache::Cache, config::SessionConfig, session::Session,
};
use librespot_oauth::{DeviceAuthClient, DeviceAuthClientBuilder, DeviceAuthorization};

/// What librespot asks for; the session-derived tokens Descant uses later
/// (spclient, pathfinder) inherit from this login.
const SCOPES: &[&str] = &[
    "streaming",
    "user-read-private",
    "user-read-email",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    "user-library-read",
    "user-library-modify",
    "user-top-read",
    "user-read-recently-played",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-follow-read",
    "user-follow-modify",
];

/// What the login screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    /// The link to encode as a QR code: the pairing page with the code in it.
    pub url: String,
    /// The code itself, for typing in at spotify.com/pair.
    pub user_code: String,
    /// How long the code is valid.
    pub expires_in: Duration,
}

/// A pairing in progress; finish it to wait for the phone.
pub struct Pairing {
    client: DeviceAuthClient,
    authorization: DeviceAuthorization,
}

/// Who is logged in, and the credentials to keep.
#[derive(Debug, Clone)]
pub struct Account {
    pub username: String,
    pub credentials: Credentials,
}

/// Asks Spotify for a pairing code.
pub async fn begin() -> Result<(PairingCode, Pairing), Error> {
    let config = SessionConfig::default();
    let client = DeviceAuthClientBuilder::new(&config.client_id, SCOPES.to_vec())
        .build()
        .map_err(|e| Error::unavailable(e.to_string()))?;
    let authorization = client
        .request_device_code_async()
        .await
        .map_err(|e| Error::unavailable(e.to_string()))?;
    let code = PairingCode {
        url: authorization.url().to_string(),
        user_code: authorization.user_code().to_string(),
        expires_in: CODE_LIFETIME,
    };
    Ok((
        code,
        Pairing {
            client,
            authorization,
        },
    ))
}

/// Codes from Spotify last an hour; the flow does not expose the exact
/// figure, so assume a little less to replace the code before it lapses.
const CODE_LIFETIME: Duration = Duration::from_secs(55 * 60);

impl Pairing {
    /// Waits until the user confirms on their phone, then trades the token for
    /// reusable credentials.
    pub async fn finish(self) -> Result<Account, Error> {
        let token = self
            .client
            .poll_for_token_async(&self.authorization)
            .await
            .map_err(|e| Error::permission_denied(e.to_string()))?;

        // A session that keeps its credentials in memory hands back the
        // reusable blob Spotify issues on login.
        let cache = Cache::new(None::<&str>, None, None, None)?.with_memory_credentials();
        let session = Session::new(SessionConfig::default(), Some(cache.clone()));
        session
            .connect(Credentials::with_access_token(token.access_token), true)
            .await?;
        let username = session.username();
        session.shutdown();
        let credentials = cache
            .credentials()
            .ok_or_else(|| Error::unavailable("Spotify did not issue reusable credentials"))?;
        Ok(Account {
            username,
            credentials,
        })
    }
}
