//! End-to-end WebSocket tests for `com.atproto.label.subscribeLabels` over a
//! real loopback listener.

use atmospheric_server::{
    ServerConfig, app,
    labeler::{EmitLabel, Labeler},
    store::LinkStore,
};
use futures_util::StreamExt;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    time::{sleep, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "atmospheric-server-stream-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn spawn(router: axum::Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (address, server)
}

async fn connect(address: SocketAddr, cursor: u64) -> Client {
    let url = format!("ws://{address}/xrpc/com.atproto.label.subscribeLabels?cursor={cursor}");
    connect_async(url).await.unwrap().0
}

async fn next_binary(socket: &mut Client) -> Vec<u8> {
    loop {
        let message = timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("timed out waiting for a label frame")
            .expect("socket ended before a label frame")
            .expect("websocket error");
        match message {
            Message::Binary(bytes) => return bytes.to_vec(),
            Message::Close(frame) => panic!("socket closed before a label frame: {frame:?}"),
            _ => {}
        }
    }
}

async fn next_close(socket: &mut Client) -> Option<CloseFrame> {
    loop {
        let message = timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("timed out waiting for close")
            .expect("socket ended before a close frame")
            .expect("websocket error");
        if let Message::Close(frame) = message {
            return frame;
        }
    }
}

async fn wait_for_subscribers(labeler: &Labeler, expected: usize) {
    for _ in 0..200 {
        if labeler.subscribers() == expected {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "expected {expected} subscribers, have {}",
        labeler.subscribers()
    );
}

fn label(uri: &str, val: &str) -> EmitLabel {
    EmitLabel {
        uri: uri.into(),
        val: val.into(),
        neg: false,
        cid: None,
        exp: None,
    }
}

fn assert_labels_frame(frame: &[u8], val: &str) {
    assert!(
        frame.starts_with(&[
            0xa2, 0x61, 0x74, 0x67, b'#', b'l', b'a', b'b', b'e', b'l', b's'
        ]),
        "frame does not start with the canonical header: {:02x?}",
        frame
    );
    assert!(
        frame
            .windows(val.len())
            .any(|window| window == val.as_bytes()),
        "frame does not contain {val}: {:02x?}",
        frame
    );
}

fn assert_error_frame(frame: &[u8], error: &str) {
    assert!(
        frame.starts_with(&[0xa1, 0x62, b'o', b'p', 0x20]),
        "frame does not start with the error header: {:02x?}",
        frame
    );
    assert!(
        frame
            .windows(error.len())
            .any(|window| window == error.as_bytes()),
        "frame does not contain {error}: {:02x?}",
        frame
    );
}

#[tokio::test]
async fn subscribe_labels_backfills_then_streams_live_labels() {
    let directory = temp_dir("subscribe");
    std::fs::create_dir_all(&directory).unwrap();
    let store = LinkStore::open(directory.join("links.json")).unwrap();
    let labeler = Labeler::open(directory.join("labeler"), "did:plc:labeler").unwrap();
    labeler
        .emit(label("did:plc:alice", "online"), 1_700_000_000)
        .await
        .unwrap();
    labeler
        .emit(label("did:plc:bob", "player"), 1_700_000_000)
        .await
        .unwrap();
    let config = ServerConfig {
        labeler: Some(labeler.clone()),
        ..ServerConfig::default()
    };
    let (address, server) = spawn(app(store, config)).await;

    let mut socket = connect(address, 0).await;
    let first = next_binary(&mut socket).await;
    assert_labels_frame(&first, "online");
    let second = next_binary(&mut socket).await;
    assert_labels_frame(&second, "player");
    socket.close(None).await.unwrap();
    wait_for_subscribers(&labeler, 0).await;

    let mut socket = connect(address, 1).await;
    let after_cursor = next_binary(&mut socket).await;
    assert_labels_frame(&after_cursor, "player");
    assert!(
        !after_cursor
            .windows("online".len())
            .any(|window| window == b"online"),
        "cursor=1 must not replay seq 1"
    );
    socket.close(None).await.unwrap();
    wait_for_subscribers(&labeler, 0).await;

    let mut socket = connect(address, 2).await;
    wait_for_subscribers(&labeler, 1).await;
    labeler
        .emit(label("did:plc:carol", "admin"), 1_700_000_000)
        .await
        .unwrap();
    let live = next_binary(&mut socket).await;
    assert_labels_frame(&live, "admin");
    socket.close(None).await.unwrap();
    wait_for_subscribers(&labeler, 0).await;

    let mut socket = connect(address, 99).await;
    let error = next_binary(&mut socket).await;
    assert_error_frame(&error, "FutureCursor");
    let close = next_close(&mut socket).await.expect("normal close frame");
    assert_eq!(close.code, CloseCode::Normal);
    assert_eq!(close.reason.as_str(), "FutureCursor");

    drop(socket);
    server.abort();
    let _ = server.await;
    let _ = std::fs::remove_dir_all(&directory);
}
