//! Credentials in the keyring, through libsecret.
//!
//! Inside the Flatpak libsecret keeps an encrypted file in the app's own data
//! directory (`keyrings/default.keyring`), its key from the Secret portal; on
//! a plain host it uses the Secret Service. Either way the credentials never
//! sit in a readable file, unlike librespot's own `credentials.json`.
//!
//! Descant used oo7 for this before. oo7 writes that same file in libsecret's
//! format (same path, header, key derivation and item encryption) and uses the
//! same Secret Service, so a sign-in stored by an older Descant reads back.

use std::collections::HashMap;

use gio::Cancellable;
use librespot_core::authentication::Credentials;
use libsecret::{Schema, SchemaAttributeType, SchemaFlags, Value};

const LABEL: &str = "Descant: Spotify account";
const ATTRIBUTES: [(&str, &str); 2] = [
    ("application", "io.github.develonrails.Descant"),
    ("kind", "spotify-credentials"),
];

/// Items stored by oo7 carry no `xdg:schema` attribute, so the schema's name
/// must stay out of the match, or the Secret Service would not find them.
fn schema() -> Schema {
    let types = ATTRIBUTES
        .iter()
        .map(|&(name, _)| (name, SchemaAttributeType::String))
        .collect();
    Schema::new(
        "io.github.develonrails.Descant",
        SchemaFlags::DONT_MATCH_NAME,
        types,
    )
}

fn attributes() -> HashMap<&'static str, &'static str> {
    ATTRIBUTES.into_iter().collect()
}

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

/// libsecret's blocking calls run a main loop of their own, so they work from
/// any thread; this keeps them off the async workers, since asking the portal
/// or unlocking the host keyring can take a moment.
async fn blocking<T, E>(
    job: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, StoreError>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    match tokio::task::spawn_blocking(job).await {
        Ok(result) => result.map_err(|e| StoreError(e.to_string())),
        Err(e) => Err(StoreError(e.to_string())),
    }
}

/// The stored credentials, if there are any.
pub async fn load() -> Result<Option<Credentials>, StoreError> {
    let secret = blocking(|| {
        libsecret::password_lookup_binary_sync(Some(&schema()), attributes(), Cancellable::NONE)
            .map(|value| value.map(|value| value.get()))
    })
    .await?;
    Ok(secret.and_then(|secret| decode(&secret)))
}

/// Replaces whatever was stored. The old item goes first: libsecret's file
/// backend only replaces an item whose attributes are exactly the same, and
/// oo7 gave its items an `xdg:content-type` attribute besides ours.
pub async fn save(credentials: &Credentials) -> Result<(), StoreError> {
    let secret = String::from_utf8(encode(credentials)).expect("JSON is UTF-8");
    blocking(move || {
        let schema = schema();
        libsecret::password_clear_sync(Some(&schema), attributes(), Cancellable::NONE)?;
        libsecret::password_store_binary_sync(
            Some(&schema),
            attributes(),
            None,
            LABEL,
            &Value::new(&secret, "text/plain"),
            Cancellable::NONE,
        )
    })
    .await
}

/// Forgets the account (log out).
pub async fn clear() -> Result<(), StoreError> {
    blocking(|| libsecret::password_clear_sync(Some(&schema()), attributes(), Cancellable::NONE))
        .await
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
