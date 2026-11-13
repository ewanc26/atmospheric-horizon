use atmospheric_server::{
    app,
    store::{LinkStore, default_links_path},
};
use std::{env, net::SocketAddr, path::PathBuf};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("atmospheric-server: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let address: SocketAddr = env::var("ATMOSPHERIC_SERVER_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8787".into())
        .parse()?;
    let data_dir = PathBuf::from(
        env::var("ATMOSPHERIC_SERVER_DATA_DIR")
            .unwrap_or_else(|_| "./atmospheric-server-data".into()),
    );
    let links_path = default_links_path(&data_dir);
    let store = LinkStore::open(&links_path)?;
    let admin_token = env::var("ATMOSPHERIC_SERVER_TOKEN")
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty());
    if admin_token.is_none() {
        eprintln!(
            "atmospheric-server: ATMOSPHERIC_SERVER_TOKEN is not set; admin writes and pairing creation are disabled"
        );
    }
    let listener = TcpListener::bind(address).await?;
    println!(
        "atmospheric-server {} listening on http://{address} (data: {})",
        env!("CARGO_PKG_VERSION"),
        links_path.display()
    );
    axum::serve(listener, app(store, admin_token))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}
