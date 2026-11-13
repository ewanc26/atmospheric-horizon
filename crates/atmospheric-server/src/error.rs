//! The server's single error type and its HTTP mapping.

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("pairing code is invalid, expired, or already used")]
    PairingUnavailable,
    #[error("admin API is disabled; set ATMOSPHERIC_SERVER_TOKEN")]
    AdminDisabled,
    #[error("missing or invalid admin bearer token")]
    Unauthorized,
    #[error("internal server error")]
    Internal,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
    #[error("corrupt links file: {0}")]
    Corrupt(String),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) | Self::PairingUnavailable => StatusCode::NOT_FOUND,
            Self::AdminDisabled => StatusCode::SERVICE_UNAVAILABLE,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Internal | Self::Io(_) | Self::Json(_) | Self::Join(_) | Self::Corrupt(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            eprintln!("atmospheric-server: {self}");
        }
        let message = if status.is_server_error() {
            "internal server error".to_owned()
        } else {
            self.to_string()
        };
        let mut response = (status, Json(json!({ "error": message }))).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}
