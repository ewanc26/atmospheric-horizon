//! Post and blob-upload actions.

use crate::{
    config::ImagePostTarget,
    error::{AtmosphericError, Result},
    session::Agent,
};
use atrium_api::types::Unknown;
use serde_json::{Value, json};

/// The largest image the worker will upload to Bluesky or Spark (the workspace
/// limit is below Spark's 5 MB lexicon cap).
pub const MAX_IMAGE_BYTES: usize = 2_000_000;

/// Grain's photo lexicon caps blobs at 1 MB.
pub const MAX_GRAIN_IMAGE_BYTES: usize = 1_000_000;

const SPARK_POST_COLLECTION: &str = "so.sprk.feed.post";
const GRAIN_PHOTO_COLLECTION: &str = "social.grain.photo";
const GRAIN_GALLERY_COLLECTION: &str = "social.grain.gallery";
const GRAIN_GALLERY_ITEM_COLLECTION: &str = "social.grain.gallery.item";

/// The byte limit for the selected image-post target.
pub fn image_size_limit(target: ImagePostTarget) -> usize {
    match target {
        ImagePostTarget::Grain => MAX_GRAIN_IMAGE_BYTES,
        ImagePostTarget::Bluesky | ImagePostTarget::Spark => MAX_IMAGE_BYTES,
    }
}

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

/// Write one already-shaped JSON record (with its `$type`) and return its
/// `at://` URI. Used by the M21 interaction records, whose small lexicon
/// shapes are built directly as JSON.
pub async fn create_record_json(
    agent: &Agent,
    did: &str,
    collection: &str,
    rkey: Option<&str>,
    record: serde_json::Value,
) -> Result<String> {
    let unknown: Unknown = serde_json::from_value(record)?;
    create_record(agent, did, collection, rkey, unknown).await
}

/// Upload raw bytes as a blob (`com.atproto.repo.uploadBlob`) and return the
/// typed blob reference (`$type: "blob"`, as the atpcraft skin lexicon and the
/// PDS both expect).
pub async fn upload_blob(agent: &Agent, bytes: Vec<u8>) -> Result<atrium_api::types::BlobRef> {
    let output = agent
        .api
        .com
        .atproto
        .repo
        .upload_blob(bytes)
        .await
        .map_err(AtmosphericError::protocol)?;
    Ok(output.data.blob)
}

/// Upsert one record at an explicit key (`com.atproto.repo.putRecord`), the
/// call atpcraft uses for its keyed records (skin, snapshots). Returns the
/// record's `at://` URI. `validate` is off, matching atpcraft.
pub async fn put_record(
    agent: &Agent,
    did: &str,
    collection: &str,
    rkey: &str,
    record: Unknown,
) -> Result<String> {
    put_record_with_validation(agent, did, collection, rkey, record, false).await
}

/// Upsert one record with PDS lexicon validation on (M65). The profile record
/// uses this so a field the lexicon rejects is reported instead of stored.
pub async fn put_record_validated(
    agent: &Agent,
    did: &str,
    collection: &str,
    rkey: &str,
    record: Unknown,
) -> Result<String> {
    put_record_with_validation(agent, did, collection, rkey, record, true).await
}

async fn put_record_with_validation(
    agent: &Agent,
    did: &str,
    collection: &str,
    rkey: &str,
    record: Unknown,
    validate: bool,
) -> Result<String> {
    let input = atrium_api::com::atproto::repo::put_record::InputData {
        collection: collection.parse().expect("valid NSID"),
        record,
        repo: did
            .parse()
            .map_err(|_| AtmosphericError::Message(format!("invalid DID: {did}")))?,
        rkey: rkey
            .parse()
            .map_err(|_| AtmosphericError::Message(format!("invalid record key: {rkey}")))?,
        swap_commit: None,
        swap_record: None,
        validate: Some(validate),
    };
    let output = agent
        .api
        .com
        .atproto
        .repo
        .put_record(input.into())
        .await
        .map_err(AtmosphericError::protocol)?;
    Ok(output.data.uri)
}

/// Delete one record (`com.atproto.repo.deleteRecord`). Used to undo follow
/// and block records (M23).
pub async fn delete_record(agent: &Agent, did: &str, collection: &str, rkey: &str) -> Result<()> {
    use atrium_api::com::atproto::repo::delete_record;
    let input = delete_record::InputData {
        collection: collection.parse().expect("valid NSID"),
        repo: did
            .parse()
            .map_err(|_| AtmosphericError::Message(format!("invalid DID: {did}")))?,
        rkey: rkey
            .parse()
            .map_err(|_| AtmosphericError::Message(format!("invalid record key: {rkey}")))?,
        swap_commit: None,
        swap_record: None,
    };
    agent
        .api
        .com
        .atproto
        .repo
        .delete_record(input.into())
        .await
        .map_err(AtmosphericError::protocol)?;
    Ok(())
}

/// Mute an account (`app.bsky.graph.muteActor`, M23). Mutes are private and
/// live on the AppView, not as a record in your repo.
pub async fn mute_actor(agent: &Agent, actor: &str) -> Result<()> {
    use atrium_api::app::bsky::graph::mute_actor;
    let actor = actor
        .parse()
        .map_err(|_| AtmosphericError::Message(format!("invalid account identifier: {actor}")))?;
    agent
        .api
        .app
        .bsky
        .graph
        .mute_actor(
            mute_actor::InputData {
                actor,
                only_quoteposts: None,
                only_reposts: None,
            }
            .into(),
        )
        .await
        .map_err(AtmosphericError::protocol)?;
    Ok(())
}

/// Unmute an account (`app.bsky.graph.unmuteActor`, M23).
pub async fn unmute_actor(agent: &Agent, actor: &str) -> Result<()> {
    use atrium_api::app::bsky::graph::unmute_actor;
    let actor = actor
        .parse()
        .map_err(|_| AtmosphericError::Message(format!("invalid account identifier: {actor}")))?;
    agent
        .api
        .app
        .bsky
        .graph
        .unmute_actor(unmute_actor::InputData { actor }.into())
        .await
        .map_err(AtmosphericError::protocol)?;
    Ok(())
}

/// Fetch one record's body (`com.atproto.repo.getRecord`), or `None` when the
/// record does not exist yet.
pub async fn get_record(
    agent: &Agent,
    did: &str,
    collection: &str,
    rkey: &str,
) -> Result<Option<Unknown>> {
    use atrium_api::com::atproto::repo::get_record;
    // getRecord is a GET: its fields travel as query parameters.
    let parameters = get_record::ParametersData {
        collection: collection.parse().expect("valid NSID"),
        repo: did
            .parse()
            .map_err(|_| AtmosphericError::Message(format!("invalid DID: {did}")))?,
        rkey: rkey
            .parse()
            .map_err(|_| AtmosphericError::Message(format!("invalid record key: {rkey}")))?,
        cid: None,
    };
    match agent
        .api
        .com
        .atproto
        .repo
        .get_record(parameters.into())
        .await
    {
        Ok(output) => Ok(Some(output.data.value)),
        // A missing record is not an error here: the caller upserts.
        Err(atrium_xrpc::Error::XrpcResponse(xrpc))
            if xrpc
                .error
                .as_ref()
                .is_some_and(|kind| kind.to_string().starts_with("RecordNotFound")) =>
        {
            Ok(None)
        }
        Err(error) => Err(AtmosphericError::protocol(error)),
    }
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

/// Build Grain's plain `{width, height}` aspect-ratio object from PNG
/// dimensions. Spark uses the same numbers but a typed def object.
fn aspect_ratio_json(dimensions: Option<(u64, u64)>, typed: bool) -> Option<Value> {
    dimensions.map(|(width, height)| {
        if typed {
            json!({
                "$type": "so.sprk.media.defs#aspectRatio",
                "width": width,
                "height": height,
            })
        } else {
            json!({ "width": width, "height": height })
        }
    })
}

/// Truncate a string to at most `max_bytes` bytes without splitting a UTF-8
/// scalar, for lexicons whose size limits are measured in bytes.
fn truncate_utf8_bytes(input: &str, max_bytes: usize) -> &str {
    if input.len() <= max_bytes {
        return input;
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    &input[..end]
}

/// Truncate to byte and character caps without splitting a UTF-8 scalar. Spark
/// caps captions at 3000 bytes and 300 graphemes; counting scalar values is a
/// conservative grapheme bound (a grapheme is one or more scalars), so no
/// extra dependency is needed to stay inside both lexicon limits.
fn truncate_utf8_bytes_and_chars(input: &str, max_bytes: usize, max_chars: usize) -> &str {
    let bytes = truncate_utf8_bytes(input, max_bytes);
    match bytes.char_indices().nth(max_chars) {
        Some((end, _)) => &bytes[..end],
        None => bytes,
    }
}

/// Build a Spark image post (`so.sprk.feed.post`) around an uploaded blob.
fn spark_post_record(
    blob: &atrium_api::types::BlobRef,
    aspect_ratio: Option<Value>,
    alt_text: &str,
    text: Option<&str>,
) -> Result<Value> {
    let mut image_obj = json!({
        "$type": "so.sprk.media.image",
        "image": blob,
        "alt": alt_text,
    });
    if let Some(aspect_ratio) = aspect_ratio {
        image_obj["aspectRatio"] = aspect_ratio;
    }
    let mut record = json!({
        "$type": SPARK_POST_COLLECTION,
        "createdAt": atrium_api::types::string::Datetime::now(),
        "media": {
            "$type": "so.sprk.media.images",
            "images": [image_obj],
        },
    });
    if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
        record["caption"] = json!({
            "$type": "so.sprk.feed.post#captionRef",
            "text": truncate_utf8_bytes_and_chars(text.trim(), 3000, 300),
        });
    }
    Ok(record)
}

/// The gallery title Grain requires: the caption when present, falling back
/// to the alt text and finally a fixed label. Grain's lexicon measures the
/// 100-character cap in bytes, so a multibyte title is truncated on a UTF-8
/// boundary.
fn grain_gallery_title(text: Option<&str>, alt_text: &str) -> String {
    let title = text
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(alt_text)
        .trim();
    let title = if title.is_empty() {
        "Screenshot from Horizon"
    } else {
        title
    };
    truncate_utf8_bytes(title, 100).to_owned()
}

/// Write one Grain gallery: a photo, the gallery that holds it, and the join
/// record that places the photo in the gallery. The gallery's AT-URI is the
/// post's public identity, so it is returned to the caller.
async fn post_grain_gallery(
    agent: &Agent,
    did: &str,
    blob: atrium_api::types::BlobRef,
    aspect_ratio: Value,
    alt_text: &str,
    text: Option<&str>,
    tid: &mut crate::tid::TidGen,
) -> Result<String> {
    let created_at = json!(atrium_api::types::string::Datetime::now());
    let photo_record = json!({
        "$type": GRAIN_PHOTO_COLLECTION,
        "photo": blob,
        "alt": alt_text,
        "aspectRatio": aspect_ratio,
        "createdAt": created_at,
    });
    let photo_uri = create_record_json(
        agent,
        did,
        GRAIN_PHOTO_COLLECTION,
        Some(&tid.next_tid()),
        photo_record,
    )
    .await?;

    let gallery_record = json!({
        "$type": GRAIN_GALLERY_COLLECTION,
        "title": grain_gallery_title(text, alt_text),
        "createdAt": created_at,
    });
    let gallery_uri = create_record_json(
        agent,
        did,
        GRAIN_GALLERY_COLLECTION,
        Some(&tid.next_tid()),
        gallery_record,
    )
    .await?;

    let item_record = json!({
        "$type": GRAIN_GALLERY_ITEM_COLLECTION,
        "createdAt": created_at,
        "gallery": gallery_uri,
        "item": photo_uri,
        "position": 0,
    });
    create_record_json(
        agent,
        did,
        GRAIN_GALLERY_ITEM_COLLECTION,
        Some(&tid.next_tid()),
        item_record,
    )
    .await?;
    Ok(gallery_uri)
}

/// Upload an image once, then write the record shape for the configured
/// network. `text` is the Bluesky caption, Spark caption or Grain gallery
/// title; the alt text always describes the image itself. Grain needs the
/// worker's TID generator because it writes three linked records.
///
/// Returns the new record's `at://` URI (the gallery URI for Grain).
pub async fn post_image(
    agent: &Agent,
    did: &str,
    image: Vec<u8>,
    alt_text: &str,
    text: Option<&str>,
    target: ImagePostTarget,
    tid: &mut crate::tid::TidGen,
) -> Result<String> {
    // The aspect ratio comes from the PNG itself, before the upload consumes
    // the bytes (JPEG/GIF/WebP posts go without one, except on Grain where it
    // is required and is rejected before the upload by the worker).
    let dimensions = png_dimensions(&image).filter(|(width, height)| *width > 0 && *height > 0);
    if target == ImagePostTarget::Grain && dimensions.is_none() {
        return Err(AtmosphericError::Message(
            "Grain requires a PNG screenshot with readable dimensions".into(),
        ));
    }
    let aspect_ratio = dimensions.and_then(|(width, height)| {
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

    match target {
        ImagePostTarget::Bluesky => {
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
        ImagePostTarget::Spark => {
            let record =
                spark_post_record(&blob, aspect_ratio_json(dimensions, true), alt_text, text)?;
            create_record_json(agent, did, SPARK_POST_COLLECTION, None, record).await
        }
        ImagePostTarget::Grain => {
            let Some(aspect_ratio) = aspect_ratio_json(dimensions, false) else {
                return Err(AtmosphericError::Message(
                    "Grain requires a PNG screenshot with readable dimensions".into(),
                ));
            };
            post_grain_gallery(agent, did, blob, aspect_ratio, alt_text, text, tid).await
        }
    }
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

    fn stub_blob() -> atrium_api::types::BlobRef {
        atrium_api::types::BlobRef::Typed(atrium_api::types::TypedBlobRef::Blob(
            atrium_api::types::Blob {
                r#ref: atrium_api::types::CidLink(
                    "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy"
                        .parse()
                        .unwrap(),
                ),
                mime_type: "image/png".into(),
                size: 1234,
            },
        ))
    }

    #[test]
    fn spark_post_record_matches_the_upstream_shape() {
        let record = spark_post_record(
            &stub_blob(),
            aspect_ratio_json(Some((2560, 1440)), true),
            "a screenshot",
            Some("  spark caption  "),
        )
        .unwrap();
        assert_eq!(record["$type"], SPARK_POST_COLLECTION);
        assert_eq!(record["caption"]["$type"], "so.sprk.feed.post#captionRef");
        assert_eq!(record["caption"]["text"], "spark caption");
        assert_eq!(record["media"]["$type"], "so.sprk.media.images");
        assert_eq!(record["media"]["images"][0]["$type"], "so.sprk.media.image");
        assert_eq!(record["media"]["images"][0]["alt"], "a screenshot");
        assert_eq!(record["media"]["images"][0]["image"]["$type"], "blob");
        assert_eq!(
            record["media"]["images"][0]["aspectRatio"]["$type"],
            "so.sprk.media.defs#aspectRatio"
        );
        assert_eq!(record["media"]["images"][0]["aspectRatio"]["width"], 2560);
        assert_eq!(record["media"]["images"][0]["aspectRatio"]["height"], 1440);

        let no_caption = spark_post_record(&stub_blob(), None, "alt", Some("  ")).unwrap();
        assert!(no_caption.get("caption").is_none());
        assert!(
            no_caption["media"]["images"][0]
                .get("aspectRatio")
                .is_none()
        );

        // Spark caps captions at 3000 bytes and 300 graphemes; 301 ASCII
        // scalars must be trimmed even though they fit the byte cap. Scalar
        // counting is a conservative bound on grapheme count.
        let long = spark_post_record(&stub_blob(), None, "alt", Some(&"x".repeat(301))).unwrap();
        assert_eq!(long["caption"]["text"].as_str().unwrap().len(), 300);
    }

    #[test]
    fn grain_title_uses_the_caption_and_respects_the_byte_cap() {
        assert_eq!(
            grain_gallery_title(Some("  from Horizon  "), "alt text"),
            "from Horizon"
        );
        assert_eq!(grain_gallery_title(None, " alt text "), "alt text");
        assert_eq!(
            grain_gallery_title(Some("   "), ""),
            "Screenshot from Horizon"
        );
        let multibyte = "é".repeat(80); // two bytes per character
        let title = grain_gallery_title(Some(&multibyte), "");
        assert_eq!(title.len(), 100);
        assert_eq!(title.chars().count(), 50);
    }

    #[test]
    fn image_size_limit_tracks_the_target_lexicons() {
        assert_eq!(image_size_limit(ImagePostTarget::Bluesky), MAX_IMAGE_BYTES);
        assert_eq!(image_size_limit(ImagePostTarget::Spark), MAX_IMAGE_BYTES);
        assert_eq!(
            image_size_limit(ImagePostTarget::Grain),
            MAX_GRAIN_IMAGE_BYTES
        );
    }
}
