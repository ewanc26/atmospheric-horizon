//! Public, read-only publishing of the bundled `at.atpcraft.*` lexicons.
//!
//! The documents are compile-time assets: a request never touches the
//! filesystem, and only an exact base NSID resolves. atpcraft appends its
//! per-server signing key id to the served NSID because its web server owns
//! that key; the identity service has no game signing key, so it serves the
//! base NSIDs.

use axum::{
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

/// The served documents, keyed by their base NSID.
const LEXICONS: &[(&str, &str)] = &[
    (
        "at.atpcraft.player.chat",
        include_str!("../../lexicons/at.atpcraft.player.chat.json"),
    ),
    (
        "at.atpcraft.player.defs",
        include_str!("../../lexicons/at.atpcraft.player.defs.json"),
    ),
    (
        "at.atpcraft.player.skin",
        include_str!("../../lexicons/at.atpcraft.player.skin.json"),
    ),
    (
        "at.atpcraft.player.snapshot",
        include_str!("../../lexicons/at.atpcraft.player.snapshot.json"),
    ),
    (
        "at.atpcraft.server.profile",
        include_str!("../../lexicons/at.atpcraft.server.profile.json"),
    ),
];

/// `GET /lexicons`: a small index of the published documents.
pub async fn index() -> Response {
    let mut links = String::new();
    for (nsid, _) in LEXICONS {
        links.push_str(&format!(
            "<li><a href=\"/lexicons/{nsid}.json\">{nsid}</a></li>"
        ));
    }
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>atmospheric lexicons</title></head><body><h1>atmospheric lexicons</h1><ul>{links}</ul></body></html>"
    );
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        body,
    )
        .into_response()
}

/// `GET /lexicons/{file}`: one bundled document, addressed by base NSID.
pub async fn document(Path(file): Path<String>) -> Response {
    let Some(nsid) = file.strip_suffix(".json") else {
        return not_found();
    };
    let Some((_, body)) = LEXICONS.iter().find(|(id, _)| *id == nsid) else {
        return not_found();
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        *body,
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Unknown lexicon.",
    )
        .into_response()
}
