//! Post and blob-upload actions.

use crate::{
    error::{AtmosphericError, Result},
    session::Agent,
};
use atrium_api::types::Unknown;

/// The largest image the worker will upload (the lexicon allows up to 2 MB).
pub const MAX_IMAGE_BYTES: usize = 2_000_000;

/// Convert a record into the `Unknown` value `create_record` expects, adding
/// the `$type` field it requires (generated `RecordData` types do not carry
/// one themselves).
pub(crate) fn record_to_unknown<T: serde::Serialize>(
    type_nsid: &str,
    record: &T,
) -> Result<Unknown> {
    let mut value = serde_json::to_value(record)?;
    let Some(map) = value.as_object_mut() else {
        return Err(AtmosphericError::Message(
            "record did not serialise to an object".into(),
        ));
    };
    map.insert("$type".into(), serde_json::Value::String(type_nsid.into()));
    Ok(serde_json::from_value(value)?)
}

/// The MIME type of an image from its magic bytes, if it is one Bluesky
/// accepts for an embed (`image/png`, `image/jpeg`, `image/gif`,
/// `image/webp`).
pub fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

/// The pixel dimensions of a PNG, from its IHDR chunk.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u64, u64)> {
    if image_mime(bytes) != Some("image/png") || bytes.len() < 24 {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?) as u64;
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?) as u64;
    Some((width, height))
}

/// Write one record to `collection` and return its `at://` URI.
async fn create_record(
    agent: &Agent,
    did: &str,
    collection: &str,
    rkey: Option<&str>,
    record: Unknown,
) -> Result<String> {
    let input = atrium_api::com::atproto::repo::create_record::InputData {
        collection: collection.parse().expect("valid NSID"),
        record,
        repo: did
            .parse()
            .map_err(|_| AtmosphericError::Message(format!("invalid DID: {did}")))?,
        rkey: rkey
            .map(|key| {
                key.parse()
                    .map_err(|_| AtmosphericError::Message(format!("invalid record key: {key}")))
            })
            .transpose()?,
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
    // The call returns `Object<OutputData>`; move the URI out of its data.
    Ok(output.data.uri)
}

/// Write a post record (`app.bsky.feed.post`) and return its `at://` URI.
async fn create_post(
    agent: &Agent,
    did: &str,
    record: atrium_api::app::bsky::feed::post::RecordData,
) -> Result<String> {
    create_record(
        agent,
        did,
        "app.bsky.feed.post",
        None,
        record_to_unknown("app.bsky.feed.post", &record)?,
    )
    .await
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
    create_post(agent, did, record).await
}

/// Upload an image (`com.atproto.repo.uploadBlob`) and post it with an
/// `app.bsky.embed.images` embed. `text` may be empty when there is an image.
///
/// Returns the new record's `at://` URI.
pub async fn post_image(
    agent: &Agent,
    did: &str,
    image: Vec<u8>,
    alt_text: &str,
    text: Option<&str>,
) -> Result<String> {
    // The aspect ratio comes from the PNG itself, before the upload consumes
    // the bytes (JPEG/GIF/WebP posts go without one).
    let aspect_ratio = png_dimensions(&image).and_then(|(width, height)| {
        let width = std::num::NonZeroU64::new(width)?;
        let height = std::num::NonZeroU64::new(height)?;
        Some(atrium_api::app::bsky::embed::defs::AspectRatio::from(
            atrium_api::app::bsky::embed::defs::AspectRatioData { height, width },
        ))
    });
    let output = agent
        .api
        .com
        .atproto
        .repo
        .upload_blob(image)
        .await
        .map_err(AtmosphericError::protocol)?;
    let blob = output.data.blob;

    let image_obj = atrium_api::app::bsky::embed::images::Image::from(
        atrium_api::app::bsky::embed::images::ImageData {
            alt: alt_text.into(),
            aspect_ratio,
            image: blob,
        },
    );
    let embed = atrium_api::app::bsky::feed::post::RecordEmbedRefs::AppBskyEmbedImagesMain(
        Box::new(atrium_api::app::bsky::embed::images::Main::from(
            atrium_api::app::bsky::embed::images::MainData {
                images: vec![image_obj],
            },
        )),
    );
    let record = atrium_api::app::bsky::feed::post::RecordData {
        created_at: atrium_api::types::string::Datetime::now(),
        embed: Some(atrium_api::types::Union::Refs(embed)),
        entities: None,
        facets: None,
        labels: None,
        langs: None,
        reply: None,
        tags: None,
        text: text.unwrap_or_default().into(),
    };
    create_post(agent, did, record).await
}

/// Publish one chat message as an `at.atpcraft.player.chat` record (the
/// atpcraft port's chat history), keyed by a fresh TID from `tid`.
///
/// Returns the new record's `at://` URI.
pub async fn post_chat(
    agent: &Agent,
    did: &str,
    server_id: &str,
    message: &str,
    tid: &mut crate::tid::TidGen,
) -> Result<String> {
    let record = crate::records::ChatRecord::new(
        server_id,
        message,
        atrium_api::types::string::Datetime::now(),
    );
    let rkey = tid.next_tid();
    create_record(
        agent,
        did,
        crate::records::CHAT_COLLECTION,
        Some(rkey.as_str()),
        record.to_unknown()?,
    )
    .await
}

/// Publish one player snapshot (`at.atpcraft.player.snapshot`), keyed by the
/// endorsing key id, and return its `at://` URI.
pub async fn post_snapshot(
    agent: &Agent,
    did: &str,
    record: &crate::records::SnapshotRecord,
    rkey: &str,
) -> Result<String> {
    create_record(
        agent,
        did,
        crate::records::SNAPSHOT_COLLECTION,
        Some(rkey),
        record.to_unknown()?,
    )
    .await
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

    #[test]
    fn image_mime_reads_magic_bytes() {
        assert_eq!(image_mime(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(image_mime(b"\xff\xd8\xff\xe0...."), Some("image/jpeg"));
        assert_eq!(image_mime(b"GIF89a...."), Some("image/gif"));
        assert_eq!(image_mime(b"RIFF\x00\x00\x00\x00WEBP"), Some("image/webp"));
        assert_eq!(image_mime(b"not an image"), None);
        assert_eq!(image_mime(b"RIFF\x00\x00\x00\x00NOPE"), None);
    }

    #[test]
    fn png_dimensions_read_the_ihdr() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&2560u32.to_be_bytes());
        png.extend_from_slice(&1440u32.to_be_bytes());
        assert_eq!(png_dimensions(&png), Some((2560, 1440)));
        assert_eq!(png_dimensions(b"\x89PNG\r\n\x1a\nshort"), None);
    }

    #[test]
    fn image_post_record_carries_the_embed_with_dollar_type() {
        let blob = atrium_api::types::BlobRef::Typed(atrium_api::types::TypedBlobRef::Blob(
            atrium_api::types::Blob {
                r#ref: atrium_api::types::CidLink(
                    "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy"
                        .parse()
                        .unwrap(),
                ),
                mime_type: "image/png".into(),
                size: 1234,
            },
        ));
        let image_obj = atrium_api::app::bsky::embed::images::Image::from(
            atrium_api::app::bsky::embed::images::ImageData {
                alt: "a screenshot".into(),
                aspect_ratio: None,
                image: blob,
            },
        );
        let embed = atrium_api::app::bsky::feed::post::RecordEmbedRefs::AppBskyEmbedImagesMain(
            Box::new(atrium_api::app::bsky::embed::images::Main::from(
                atrium_api::app::bsky::embed::images::MainData {
                    images: vec![image_obj],
                },
            )),
        );
        let record = atrium_api::app::bsky::feed::post::RecordData {
            created_at: atrium_api::types::string::Datetime::now(),
            embed: Some(atrium_api::types::Union::Refs(embed)),
            entities: None,
            facets: None,
            labels: None,
            langs: None,
            reply: None,
            tags: None,
            text: "from Horizon".into(),
        };
        let unknown = record_to_unknown("app.bsky.feed.post", &record).unwrap();
        let value = serde_json::to_value(&unknown).unwrap();
        assert_eq!(value["$type"], "app.bsky.feed.post");
        assert_eq!(value["embed"]["$type"], "app.bsky.embed.images");
        assert_eq!(value["embed"]["images"][0]["alt"], "a screenshot");
        assert_eq!(value["embed"]["images"][0]["image"]["$type"], "blob");
        assert_eq!(value["text"], "from Horizon");
    }
}
