//! Spotify's internal GraphQL API ("pathfinder"), as the official client uses it.
//!
//! Spike S3/S4 found it answers with the session's login5 token and client
//! token, where the Web API refuses everything. Queries are persisted: each
//! operation is named and pinned by a hash. Spotify changes those hashes now
//! and then, so all of them live in this one table.

use librespot_core::{Error, session::Session};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    lenient::{Shape, lenient},
    net,
};

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

/// Podcasts, from the web player itself (open.spotify.com, 4 November 2026;
/// spike S7). Shows only: 19 KB, where the current searchDesktop sends
/// 158 KB to add them; the search above already brings the episodes.
pub const SEARCH_PODCASTS: Operation = Operation {
    name: "searchPodcasts",
    hash: "0195d9f61b43606d490bca64c3456e3593528cea6cc05c7e822c7c42beed0f4e",
};

/// A show's page: name, maker, description and whether it is followed.
pub const SHOW_METADATA: Operation = Operation {
    name: "queryShowMetadataV2",
    hash: "b475447846f37cc426add800b995a7859c169c53c72b62de6338c0c40a5dacea",
};

/// A page of a show's episodes, newest first, each with the listener's place.
pub const PODCAST_EPISODES: Operation = Operation {
    name: "queryPodcastEpisodes",
    hash: "3539d746cf882f3909660de40b4ef472b3f5893a0761d617c282541de5d412c5",
};

const ENDPOINT: &str = "https://api-partner.spotify.com/pathfinder/v2/query";

/// Runs `operation` with `variables` and reads the answer's `data` as `T`,
/// which names only the fields its caller shows.
pub(crate) async fn query<T: Shape>(
    session: &Session,
    operation: &Operation,
    variables: Value,
) -> Result<T, Error> {
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
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::unavailable(e.to_string()))?;
    read(operation.name, &bytes)
}

/// A pathfinder answer: `data`, unless Spotify names `errors`.
#[derive(Deserialize)]
#[serde(bound = "T: Shape")]
struct Answer<T> {
    #[serde(default, deserialize_with = "lenient")]
    data: T,
    #[serde(default, deserialize_with = "present")]
    errors: Option<Value>,
}

/// `errors` counts once it is there, even as `null`.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// Reads an answer to `operation`; a missing or null `data` is `T`'s default.
pub(crate) fn read<T: Shape>(operation: &str, body: &[u8]) -> Result<T, Error> {
    let answer: Answer<T> =
        serde_json::from_slice(body).map_err(|e| Error::unavailable(e.to_string()))?;
    if let Some(errors) = answer.errors {
        return Err(Error::unavailable(format!("{operation}: {errors}")));
    }
    Ok(answer.data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lenient::{Items, shape};

    shape! {
        struct Found {
            items: Vec<Option<String>>,
        }
    }

    shape! {
        struct Data {
            found: Items<Found>,
        }
    }

    #[test]
    fn data_is_read_and_the_rest_skipped() {
        let body = br#"{"data": {"found": {"items": [{"items": ["a"]}]}, "extra": [1, {}]},
                        "extensions": {"cost": 3}}"#;
        let data: Data = read("op", body).unwrap();
        assert_eq!(data.found.items[0].items, vec![Some("a".to_string())]);
    }

    #[test]
    fn errors_fail_the_answer_even_beside_data() {
        let body = br#"{"data": {"found": null},
                        "errors": [{"message": "PersistedQueryNotFound"}]}"#;
        let err = read::<Data>("searchDesktop", body).err().unwrap();
        assert!(err.to_string().contains("searchDesktop"));
        assert!(err.to_string().contains("PersistedQueryNotFound"));
    }

    #[test]
    fn missing_or_null_data_is_nothing_found() {
        for body in [&br#"{}"#[..], br#"{"data": null}"#, br#"{"data": "odd"}"#] {
            assert!(read::<Data>("op", body).unwrap().found.items.is_empty());
        }
        assert!(read::<Data>("op", b"not json").is_err());
    }
}
