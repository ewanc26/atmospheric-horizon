use super::AppState;
use crate::{
    error::ApiError,
    model::{
        BrowserPairingInput, BrowserPairingResponse, normalize_player_name, normalize_uuid,
        pairing_ttl,
    },
    oauth::OAuthSession,
    page::{browser_page, html_escape},
    skin::{COLLECTION, PendingSkin, SkinSigner, rkey, validate_skin},
};
use atmospheric::records::SkinModel;
use atrium_api::types::BlobRef;
use axum::{
    Json,
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;

/// Start a one-time browser skin upload for an already-linked player.
pub(super) async fn create_skin_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<BrowserPairingInput>,
) -> Result<(StatusCode, Json<BrowserPairingResponse>), ApiError> {
    state.authorize_link(&headers)?;
    if state.skin_signer.is_none() {
        return Err(ApiError::SkinDisabled);
    }
    let player_uuid = normalize_uuid(&input.player_uuid)?;
    let player_name = normalize_player_name(&input.player_name)?;
    let ttl = pairing_ttl(input.ttl_seconds)?;
    let link = state.store.link(&player_uuid).await?.ok_or_else(|| {
        ApiError::Invalid("link your Bluesky account before uploading a skin".into())
    })?;
    let (token, expires_at) = state.skin_uploads.create(
        &player_uuid,
        &player_name,
        &link.did,
        link.handle.as_deref(),
        ttl,
    );
    let public_url = state
        .public_url
        .as_deref()
        .ok_or(ApiError::BrowserDisabled)?;
    Ok((
        StatusCode::CREATED,
        Json(BrowserPairingResponse {
            url: format!("{public_url}/skin/{token}"),
            expires_at,
        }),
    ))
}

pub(super) async fn skin_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Response {
    let Some(pending) = state.skin_uploads.pending(&token) else {
        return browser_page(
            StatusCode::NOT_FOUND,
            "Upload link expired",
            "This skin upload link has expired or was already used. Ask the server for a new one in game.",
        );
    };
    skin_form(&token, &pending, None, StatusCode::OK)
}

pub(super) async fn skin_upload(
    State(state): State<AppState>,
    Path(token): Path<String>,
    mut multipart: Multipart,
) -> Response {
    let Some(pending) = state.skin_uploads.pending(&token) else {
        return browser_page(
            StatusCode::NOT_FOUND,
            "Upload link expired",
            "This skin upload link has expired or was already used. Ask the server for a new one in game.",
        );
    };
    let Some(client) = &state.skin_oauth else {
        return browser_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Skin upload is disabled",
            "The server has no public URL configured.",
        );
    };
    let Some(_signer) = &state.skin_signer else {
        return browser_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Skin upload is disabled",
            "The server has no skin signing key configured.",
        );
    };

    let mut model = "default".to_string();
    let mut handle = pending.handle.clone().unwrap_or_default();
    let mut bytes = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => {
                return skin_form(
                    &token,
                    &pending,
                    Some("The upload was not valid multipart form data."),
                    StatusCode::BAD_REQUEST,
                );
            }
        };
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "model" => {
                if let Ok(value) = field.text().await {
                    model = value;
                }
            }
            "handle" => {
                if let Ok(value) = field.text().await {
                    handle = value;
                }
            }
            "skin" => match field.bytes().await {
                Ok(value) => bytes = Some(value.to_vec()),
                Err(_) => {
                    return skin_form(
                        &token,
                        &pending,
                        Some("Could not read the uploaded PNG."),
                        StatusCode::BAD_REQUEST,
                    );
                }
            },
            _ => {}
        }
    }
    let Some(bytes) = bytes else {
        return skin_form(
            &token,
            &pending,
            Some("Choose a skin PNG first."),
            StatusCode::BAD_REQUEST,
        );
    };
    if let Err(message) = validate_skin(&bytes) {
        return skin_form(&token, &pending, Some(message), StatusCode::BAD_REQUEST);
    }
    let model = SkinModel::from_key(&model).unwrap_or(SkinModel::Default);
    let handle = handle.trim().trim_start_matches('@').to_ascii_lowercase();
    if handle.is_empty() || !handle.contains('.') {
        return skin_form(
            &token,
            &pending,
            Some("Enter the Bluesky handle that owns this player."),
            StatusCode::BAD_REQUEST,
        );
    }
    let started = match client.start(&handle).await {
        Ok(started) => started,
        Err(error) => {
            eprintln!("atmospheric-server: skin OAuth start failed: {error}");
            return skin_form(
                &token,
                &pending,
                Some("Could not start Bluesky sign-in. Check the handle and try again."),
                StatusCode::BAD_GATEWAY,
            );
        }
    };
    state
        .skin_uploads
        .begin_auth(&token, model.key(), bytes, started.pending);
    let target = html_escape(&started.authorize_url);
    browser_page(
        StatusCode::OK,
        "Continue to Bluesky",
        &format!(
            "<p>Sign in as <strong>{}</strong> to publish this skin.</p>\
             <p><a href=\"{target}\">Continue</a></p>\
             <meta http-equiv=\"refresh\" content=\"0;url={target}\">",
            html_escape(&handle)
        ),
    )
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    iss: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

pub(super) async fn skin_callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if let Some(error) = query.error.as_deref() {
        let description = query
            .error_description
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(error);
        return browser_page(
            StatusCode::BAD_REQUEST,
            "Bluesky sign-in failed",
            &html_escape(description),
        );
    }
    let (Some(code), Some(auth_state)) = (query.code.as_deref(), query.state.as_deref()) else {
        return browser_page(
            StatusCode::BAD_REQUEST,
            "Incomplete sign-in",
            "Bluesky did not return the expected authorization response. Start again from the upload link.",
        );
    };
    let Some(auth) = state.skin_uploads.take_auth(auth_state) else {
        return browser_page(
            StatusCode::BAD_REQUEST,
            "Sign-in expired",
            "This sign-in attempt expired or was already used. Start again from the upload link.",
        );
    };
    let Some(pending) = state.skin_uploads.pending(&auth.token) else {
        return browser_page(
            StatusCode::GONE,
            "Upload link expired",
            "This upload link expired before sign-in finished. Ask the server for a new one in game.",
        );
    };
    let Some(client) = &state.skin_oauth else {
        return browser_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Skin upload is disabled",
            "The server has no public URL configured.",
        );
    };
    let session = match client
        .finish_session(&auth.authorization, code, query.iss.as_deref())
        .await
    {
        Ok(session) => session,
        Err(error) => {
            eprintln!("atmospheric-server: skin OAuth callback failed: {error}");
            return browser_page(
                StatusCode::BAD_GATEWAY,
                "Could not finish sign-in",
                "The authorization service refused this sign-in. Start again from the upload link.",
            );
        }
    };
    if session.did != pending.did {
        return browser_page(
            StatusCode::FORBIDDEN,
            "Wrong Bluesky account",
            &format!(
                "This player is linked to <strong>{}</strong>. Sign in as that account, or relink the player in game.",
                html_escape(&pending.did)
            ),
        );
    }
    let signer = match &state.skin_signer {
        Some(signer) => signer,
        None => {
            return browser_page(
                StatusCode::SERVICE_UNAVAILABLE,
                "Skin upload is disabled",
                "The server has no skin signing key configured.",
            );
        }
    };
    let model = SkinModel::from_key(&auth.model).unwrap_or(SkinModel::Default);
    let result = publish(&session, signer, &auth.bytes, model).await;
    match result {
        Ok(uri) => {
            state.skin_uploads.consume(&auth.token);
            browser_page(
                StatusCode::OK,
                "Skin published",
                &format!(
                    "<p>Your skin was uploaded to your PDS and endorsed for this server.</p>\
                     <p>Record: <code>{}</code></p>\
                     <p>In-game application is not available yet on Horizon; it will appear after the native skin setter is available.</p>\
                     <p>You can close this page and return to the game.</p>",
                    html_escape(&uri)
                ),
            )
        }
        Err(error) => {
            eprintln!("atmospheric-server: skin publish failed: {error}");
            browser_page(
                StatusCode::BAD_GATEWAY,
                "Could not publish the skin",
                "The PDS refused the skin record. Start again from the upload link, or check that your session and PDS allow blob and repo writes.",
            )
        }
    }
}

async fn publish(
    session: &OAuthSession,
    signer: &SkinSigner,
    bytes: &[u8],
    model: SkinModel,
) -> Result<String, String> {
    let uploaded = session.upload_blob(bytes).await?;
    let blob: BlobRef = serde_json::from_value(
        uploaded
            .get("blob")
            .cloned()
            .ok_or_else(|| "uploadBlob returned no blob".to_string())?,
    )
    .map_err(|error| format!("uploadBlob returned an invalid blob: {error}"))?;
    let existing = session
        .get_record(COLLECTION, rkey(signer.key_id()))
        .await?;
    let existing_value = existing.as_ref().and_then(|value| value.get("value"));
    let record = signer.build_record(&session.did, blob, existing_value, model);
    let value = serde_json::to_value(record.to_unknown().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let written = session
        .put_record(COLLECTION, rkey(signer.key_id()), value)
        .await?;
    written
        .get("uri")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| "putRecord returned no uri".into())
}

fn skin_form(
    token: &str,
    pending: &PendingSkin,
    error: Option<&str>,
    status: StatusCode,
) -> Response {
    let action = html_escape(&format!("/skin/{token}"));
    let handle = pending.handle.as_deref().unwrap_or_default();
    let error = error
        .map(|message| format!("<p class=\"error\">{}</p>", html_escape(message)))
        .unwrap_or_default();
    browser_page(
        status,
        "Upload your skin",
        &format!(
            "<style>\
             body{{font-family:system-ui,sans-serif;background:#111;color:#ddd;margin:0}}\
             main{{max-width:34rem;margin:3rem auto;padding:1.5rem;border:1px solid #333;background:#181818}}\
             h1{{color:#58a6ff}}label{{display:block;margin:1rem 0 .35rem;font-weight:600}}\
             input,select,button{{font:inherit;padding:.65rem;width:100%;box-sizing:border-box;background:#222;color:#ddd;border:1px solid #444}}\
             button{{margin-top:1.25rem;background:#58a6ff;color:#111;border:0;font-weight:700;cursor:pointer}}\
             .error{{color:#f38ba8}}.hint{{color:#999;font-size:.9rem}}</style>\
             <p>Upload a 64×64 or 64×32 PNG for <strong>{}</strong>.</p>\
             {error}\
             <form method=\"post\" action=\"{action}\" enctype=\"multipart/form-data\">\
             <label for=\"handle\">Bluesky handle</label>\
             <input id=\"handle\" name=\"handle\" type=\"text\" autocomplete=\"username\" value=\"{}\" required>\
             <label for=\"model\">Model</label>\
             <select id=\"model\" name=\"model\"><option value=\"default\">Classic</option><option value=\"slim\">Slim</option></select>\
             <label for=\"skin\">Skin PNG (maximum 1 MiB)</label>\
             <input id=\"skin\" name=\"skin\" type=\"file\" accept=\"image/png\" required>\
             <button type=\"submit\">Sign in and publish</button>\
             </form>\
             <p class=\"hint\">This link is single-use and expires automatically. The existing cape and other servers' endorsements are preserved.</p>",
            html_escape(&pending.player_name),
            html_escape(handle)
        ),
    )
}
