//! Credentials in the keyring.
//!
//! Inside the Flatpak oo7 uses the Secret portal (an encrypted file only this
//! app can read); on a plain host it uses the Secret Service. Either way the
//! credentials never sit in a readable file, unlike librespot's own
//! `credentials.json`.

use librespot_core::authentication::Credentials;

const LABEL: &str = "Descant: Spotify account";
const ATTRIBUTES: &[(&str, &str)] = &[
    ("application", "io.github.develonrails.Descant"),
    ("kind", "spotify-credentials"),
];

/// The secret as stored: librespot's own JSON form of the credentials.
pub fn encode(credentials: &Credentials) -> Vec<u8> {
    serde_json::to_vec(credentials).expect("credentials always serialise")
}

pub fn decode(secret: &[u8]) -> Option<Credentials> {
    serde_json::from_slice(secret).ok()
}

#[derive(Debug)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "keyring: {}", self.0)
    }
}

impl From<oo7::Error> for StoreError {
    fn from(e: oo7::Error) -> StoreError {
        StoreError(e.to_string())
    }
}

/// The stored credentials, if there are any.
pub async fn load() -> Result<Option<Credentials>, StoreError> {
    let keyring = oo7::Keyring::new().await?;
    keyring.unlock().await?;
    for item in keyring.search_items(&ATTRIBUTES).await? {
        if let Some(credentials) = decode(&item.secret().await?) {
            return Ok(Some(credentials));
        }
    }
    Ok(None)
}

pub async fn save(credentials: &Credentials) -> Result<(), StoreError> {
    let keyring = oo7::Keyring::new().await?;
    keyring.unlock().await?;
    keyring
        .create_item(LABEL, &ATTRIBUTES, encode(credentials), true)
        .await?;
    Ok(())
}

/// Forgets the account (log out).
pub async fn clear() -> Result<(), StoreError> {
    let keyring = oo7::Keyring::new().await?;
    keyring.delete(&ATTRIBUTES).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_survive_the_round_trip() {
        let credentials = Credentials::with_password("someone", "secret-blob");
        assert_eq!(decode(&encode(&credentials)), Some(credentials));
    }

    #[test]
    fn a_damaged_secret_is_no_credentials_rather_than_a_crash() {
        assert_eq!(decode(b"not json"), None);
        assert_eq!(decode(b""), None);
    }
}
