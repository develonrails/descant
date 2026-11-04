//! Spotify's internal GraphQL API ("pathfinder"), as the official client uses it.
//!
//! Spike S3/S4 found it answers with the session's login5 token and client
//! token, where the Web API refuses everything. Queries are persisted: each
//! operation is named and pinned by a hash. Spotify changes those hashes now
//! and then, so all of them live in this one table.

use librespot_core::{Error, session::Session};
use serde_json::{Value, json};

use crate::net;

/// An operation and the hash of its persisted query.
pub struct Operation {
    pub name: &'static str,
    pub hash: &'static str,
}

/// Search, as Spotify's desktop app searches (hash as used by
/// sjdonado/idonthavespotify, MIT).
pub const SEARCH_DESKTOP: Operation = Operation {
    name: "searchDesktop",
    hash: "75bbf6bfcfdf85b8fc828417bfad92b7cd66bf7f556d85670f4da8292373ebec",
};

/// A section of Home, such as Made for You (hash as used by Spotifast, MIT).
pub const HOME_SECTION: Operation = Operation {
    name: "homeSection",
    hash: "eb3fba2d388cf4fc4d696b1757a58584e9538a3b515ea742e9cc9465807340be",
};

/// The user's top artists and tracks (hash as measured by ralph/Spotifly, MIT).
pub const USER_TOP_CONTENT: Operation = Operation {
    name: "userTopContent",
    hash: "61b7353d3625780022dbc63d459eb64cd609e7e5e2a820f9160d65030efe2e15",
};

const ENDPOINT: &str = "https://api-partner.spotify.com/pathfinder/v2/query";

/// Runs `operation` with `variables` and returns the response's `data`.
pub async fn query(
    session: &Session,
    operation: &Operation,
    variables: Value,
) -> Result<Value, Error> {
    let body = json!({
        "operationName": operation.name,
        "variables": variables,
        "extensions": {"persistedQuery": {"version": 1, "sha256Hash": operation.hash}},
    });
    let response = net::authed(session, reqwest::Method::POST, ENDPOINT)
        .await?
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::unavailable(e.without_url().to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(Error::resource_exhausted("Spotify asks to slow down"));
    }
    if !status.is_success() {
        return Err(Error::unavailable(format!(
            "{} answered {status}",
            operation.name
        )));
    }
    let mut value: Value = response
        .json()
        .await
        .map_err(|e| Error::unavailable(e.to_string()))?;
    if let Some(errors) = value.get("errors") {
        return Err(Error::unavailable(format!("{}: {errors}", operation.name)));
    }
    Ok(value["data"].take())
}
