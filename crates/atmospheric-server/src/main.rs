use atmospheric_server::{
    ServerConfig, app,
    labeler::Labeler,
    skin::SkinSigner,
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
    let link_token = env::var("ATMOSPHERIC_SERVER_LINK_TOKEN")
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty());
    let public_url = env::var("ATMOSPHERIC_SERVER_PUBLIC_URL")
        .ok()
        .map(|url| url.trim().trim_end_matches('/').to_owned())
        .filter(|url| !url.is_empty());
    if link_token.is_some() && public_url.is_some() {
        println!(
            "atmospheric-server: browser linking enabled at {}",
            public_url.as_deref().unwrap_or_default()
        );
    } else {
        eprintln!(
            "atmospheric-server: browser linking is disabled; set ATMOSPHERIC_SERVER_PUBLIC_URL and ATMOSPHERIC_SERVER_LINK_TOKEN"
        );
    }
    let skin_signer = env::var("ATMOSPHERIC_SERVER_SKIN_KEY")
        .ok()
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .map(|path| -> Result<SkinSigner, Box<dyn std::error::Error>> {
            let raw = std::fs::read_to_string(&path)?;
            let file: atmospheric::signing::SigningKeyFile = serde_json::from_str(&raw)?;
            let signer = SkinSigner::from_file(file)
                .map_err(|error| std::io::Error::other(format!("skin signing key: {error}")))?;
            println!(
                "atmospheric-server: browser skin upload enabled (key {})",
                signer.key_id()
            );
            Ok(signer)
        })
        .transpose()?;
    if skin_signer.is_none() {
        eprintln!(
            "atmospheric-server: browser skin upload disabled; set ATMOSPHERIC_SERVER_SKIN_KEY to the game's signing-key.json"
        );
    }
    let labeler_did = env::var("ATMOSPHERIC_SERVER_LABELER_DID")
        .ok()
        .map(|did| did.trim().to_owned())
        .filter(|did| !did.is_empty());
    let labeler = match labeler_did {
        Some(did) => {
            let labeler = Labeler::open(data_dir.join("labeler"), &did)?;
            println!(
                "atmospheric-server: labeler enabled as {did} (signing key {})",
                labeler.did_key()
            );
            Some(labeler)
        }
        None => {
            eprintln!("atmospheric-server: labeler disabled; set ATMOSPHERIC_SERVER_LABELER_DID");
            None
        }
    };
    let listener = TcpListener::bind(address).await?;
    println!(
        "atmospheric-server {} listening on http://{address} (data: {})",
        env!("CARGO_PKG_VERSION"),
        links_path.display()
    );
    let config = ServerConfig {
        admin_token,
        link_token,
        public_url,
        oauth_resolver: None,
        labeler,
        skin_signer,
    };
    axum::serve(listener, app(store, config))
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
