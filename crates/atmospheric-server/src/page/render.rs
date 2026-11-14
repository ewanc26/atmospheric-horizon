use super::html_escape;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

pub(crate) fn browser_page(status: StatusCode, title: &str, body: &str) -> Response {
    (
        status,
        Html(format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>{}</title></head><body><main><h1>{}</h1>{}</main></body></html>",
            html_escape(title),
            html_escape(title),
            body
        )),
    )
        .into_response()
}
