//! Prints the signed-in account's profile, with the stored spike credentials.
use std::{env, path::PathBuf};

use librespot_core::{cache::Cache, config::SessionConfig, session::Session};

#[tokio::main]
async fn main() {
    let dir = PathBuf::from(env::var_os("HOME").unwrap()).join(".cache/descant-spikes/creds");
    let credentials = Cache::new(Some(dir), None, None, None)
        .unwrap()
        .credentials()
        .unwrap();
    let session = Session::new(SessionConfig::default(), None);
    session.connect(credentials, false).await.unwrap();
    println!("{:?}", descant_spotify::profile::me(&session).await);
}
