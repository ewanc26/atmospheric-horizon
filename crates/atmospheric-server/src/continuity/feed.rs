//! PLC export stream and HTTP catch-up client (#190).
//!
//! The WebSocket is a latency optimisation only: a lost stream is retried and
//! the jittered DID poll keeps continuity observations fresh without it.

use super::model::{ExportOperation, export_page_url, export_stream_url, parse_export_line};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, protocol::CloseFrame},
};

/// The concrete stream type returned by `connect`.
pub type PlcStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// One decoded item from the export stream.
#[derive(Debug)]
pub enum FeedMessage {
    Operation(ExportOperation),
    Closed(Option<String>),
    Ended,
}

/// Open a stream from the configured PLC directory or mirror, optionally
/// resuming after a sequence cursor.
pub async fn connect(directory: &str, cursor: Option<u64>) -> Result<PlcStream, String> {
    let url = export_stream_url(directory, cursor)?;
    let (stream, _) = connect_async(url)
        .await
        .map_err(|error| format!("PLC export stream: {error}"))?;
    Ok(stream)
}

/// Read the next export operation, answering WebSocket pings along the way.
pub async fn next(stream: &mut PlcStream) -> Result<FeedMessage, String> {
    loop {
        let Some(frame) = stream.next().await else {
            return Ok(FeedMessage::Ended);
        };
        match frame.map_err(|error| format!("PLC export stream: {error}"))? {
            Message::Text(text) => {
                let Some(operation) = parse_export_line(&text)
                    .map_err(|error| format!("bad PLC export frame: {error}"))?
                else {
                    continue;
                };
                return Ok(FeedMessage::Operation(operation));
            }
            Message::Binary(bytes) => {
                let text = String::from_utf8(bytes.to_vec())
                    .map_err(|error| format!("bad PLC export frame: {error}"))?;
                let Some(operation) = parse_export_line(&text)
                    .map_err(|error| format!("bad PLC export frame: {error}"))?
                else {
                    continue;
                };
                return Ok(FeedMessage::Operation(operation));
            }
            Message::Ping(payload) => {
                stream
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|error| format!("PLC export stream pong: {error}"))?;
            }
            Message::Close(frame) => {
                return Ok(FeedMessage::Closed(close_reason(frame.as_ref())));
            }
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

fn close_reason(frame: Option<&CloseFrame>) -> Option<String> {
    let reason: &str = frame?.reason.as_ref();
    if reason.is_empty() {
        None
    } else {
        Some(reason.to_string())
    }
}

/// Fetch one sequence-ordered export page, used when a stream cursor is too
/// old or the WebSocket is unavailable.
pub async fn export_page(
    http: &Client,
    directory: &str,
    after: u64,
) -> Result<Vec<ExportOperation>, String> {
    let url = export_page_url(directory, after)?;
    let response = http
        .get(&url)
        .send()
        .await
        .map_err(|error| format!("PLC export request: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("PLC export response: {error}"))?;
    if !status.is_success() {
        return Err(format!("PLC export HTTP {status}: {body}"));
    }
    let mut operations = Vec::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(operation) =
            parse_export_line(line).map_err(|error| format!("bad PLC export line: {error}"))?
        {
            operations.push(operation);
        }
    }
    Ok(operations)
}
