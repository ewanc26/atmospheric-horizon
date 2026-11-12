//! Post and blob-upload actions.

use crate::{
    error::{AtmosphericError, Result},
    session::Agent,
};
use atrium_api::types::Unknown;

/// Convert a record into the `Unknown` value `create_record` expects, adding
/// the `$type` field it requires (generated `RecordData` types do not carry
/// one themselves).
fn record_to_unknown<T: serde::Serialize>(type_nsid: &str, record: &T) -> Result<Unknown> {
    let mut value = serde_json::to_value(record)?;
    let Some(map) = value.as_object_mut() else {
        return Err(AtmosphericError::Message(
            "record did not serialise to an object".into(),
        ));
    };
    map.insert("$type".into(), serde_json::Value::String(type_nsid.into()));
    Ok(serde_json::from_value(value)?)
}

/// Post plain text to Bluesky (`app.bsky.feed.post`).
///
/// Returns the new record's `at://` URI.
pub async fn post_text(agent: &Agent, did: &str, text: &str) -> Result<String> {
    let record = atrium_api::app::bsky::feed::post::RecordData {
        created_at: atrium_api::types::string::Datetime::now(),
        embed: None,
        entities: None,
        facets: None,
        labels: None,
        langs: None,
        reply: None,
        tags: None,
        text: text.into(),
    };
    let input = atrium_api::com::atproto::repo::create_record::InputData {
        collection: "app.bsky.feed.post".parse().expect("valid NSID"),
        record: record_to_unknown("app.bsky.feed.post", &record)?,
        repo: did
            .parse()
            .map_err(|_| AtmosphericError::Message(format!("invalid DID: {did}")))?,
        rkey: None,
        swap_commit: None,
        validate: None,
    };
    let output = agent
        .api
        .com
        .atproto
        .repo
        .create_record(input.into())
        .await
        .map_err(AtmosphericError::protocol)?;
    Ok(output.uri)
}

/// Upload an image file and post it as an embedded image.
///
/// TODO(M3): `com.atproto.repo.uploadBlob` plus an
/// `app.bsky.embed.images` record; fall back to text-only on failure.
pub async fn post_image(
    _agent: &Agent,
    _did: &str,
    _path: &str,
    _alt_text: &str,
    _text: Option<&str>,
) -> Result<String> {
    todo!("screenshot posting lands in M3")
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_api::types::TryFromUnknown;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Fixture {
        text: String,
        camel_case_field: u8,
    }

    #[test]
    fn record_conversion_adds_dollar_type_and_round_trips() {
        let record = Fixture {
            text: "hello".into(),
            camel_case_field: 7,
        };
        let unknown = record_to_unknown("com.example.record", &record).unwrap();

        // The $type field is present.
        assert!(matches!(
            &unknown,
            Unknown::Object(map) if map.get("$type").is_some()
        ));

        // The record survives a round trip through the unknown value.
        let back = Fixture::try_from_unknown(unknown).unwrap();
        assert_eq!(back, record);
    }
}
