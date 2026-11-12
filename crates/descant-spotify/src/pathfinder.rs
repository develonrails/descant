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
    let value: Value = response
        .json()
        .await
        .map_err(|e| Error::unavailable(e.to_string()))?;
    answer(operation.name, value)
}

/// A response's `data`, as long as some of it came.
///
/// GraphQL answers what it can: when one field fails (search's featured top
/// result, say) the rest still comes, with the failure under `errors`. Only
/// when nothing at all came is the query a failure.
fn answer(name: &str, mut value: Value) -> Result<Value, Error> {
    let data = value["data"].take();
    let errors = value.get("errors").filter(|e| !e.is_null());
    let some = data
        .as_object()
        .is_some_and(|fields| fields.values().any(|v| !v.is_null()));
    match errors {
        Some(errors) if !some => Err(Error::unavailable(format!("{name}: {errors}"))),
        Some(errors) => {
            log::warn!("{name}, in part: {errors}");
            Ok(data)
        }
        None => Ok(data),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_that_failed_leaves_the_rest_of_the_answer() {
        let data = answer(
            "searchDesktop",
            json!({
                "data": {"search": {"artists": {"items": [{"uri": "spotify:artist:r"}]},
                                    "topResults": {"featured": null}}},
                "errors": [{"message": "Exception while fetching data \
                            (/search/topResults/featured) : Server error",
                            "path": ["search", "topResults", "featured"]}]
            }),
        )
        .expect("the rest of the answer");
        assert_eq!(
            data["search"]["artists"]["items"][0]["uri"],
            "spotify:artist:r"
        );
    }

    #[test]
    fn an_answer_with_nothing_in_it_is_a_failure() {
        let failed = json!([{"message": "Server error"}]);
        assert!(answer("q", json!({"errors": failed})).is_err());
        assert!(answer("q", json!({"data": null, "errors": failed})).is_err());
        assert!(answer("q", json!({"data": {"search": null}, "errors": failed})).is_err());
    }

    #[test]
    fn an_answer_without_errors_is_its_data() {
        assert_eq!(
            answer("q", json!({"data": {"a": 1}})).unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            answer("q", json!({"data": {"a": 1}, "errors": null})).unwrap(),
            json!({"a": 1})
        );
    }
}
