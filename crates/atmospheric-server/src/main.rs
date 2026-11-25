use atmospheric_server::{
    ServerConfig, app,
    continuity::{Continuity, ContinuitySettings, Watcher},
    decision::{DecisionLedger, DecisionSettings},
    labeler::Labeler,
    operator::{DEFAULT_GRANT_TTL_SECS, GrantScope, Operator, OperatorSettings, public_verify},
    skin::SkinSigner,
    store::{LinkStore, default_links_path},
};
use std::{
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
};
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
    let operator = open_operator(&data_dir)?;
    let continuity = open_continuity()?;
    let ledger = open_ledger(&data_dir)?;
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
        operator,
        continuity,
        ledger,
    };
    axum::serve(listener, app(store, config))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Build the experimental DID operator layer from the environment. An empty
/// allowlist leaves the feature completely off and creates no key or log.
fn open_operator(data_dir: &Path) -> Result<Option<Operator>, Box<dyn std::error::Error>> {
    let settings = parse_operator_settings(
        env::var("ATMOSPHERIC_SERVER_OPERATOR_DIDS").ok(),
        env::var("ATMOSPHERIC_SERVER_OPERATOR_GRANT_TTL_SECONDS").ok(),
        env::var("ATMOSPHERIC_SERVER_OPERATOR_SCOPES").ok(),
    )
    .map_err(std::io::Error::other)?;
    let Some(settings) = settings else {
        eprintln!(
            "atmospheric-server: DID operator grants disabled; set ATMOSPHERIC_SERVER_OPERATOR_DIDS to a comma-separated allowlist"
        );
        return Ok(None);
    };
    let resolver = atmospheric::oauth::Resolver {
        handle_service: env::var("ATMOSPHERIC_SERVER_OPERATOR_HANDLE_SERVICE")
            .unwrap_or_else(|_| "https://bsky.social".into()),
        plc_directory: env::var("ATMOSPHERIC_SERVER_OPERATOR_PLC_DIRECTORY")
            .unwrap_or_else(|_| "https://plc.directory".into()),
    };
    let did_count = settings.dids.len();
    let operator = Operator::open(
        data_dir.join("operator"),
        settings,
        public_verify(reqwest::Client::new(), resolver),
    )?;
    println!(
        "atmospheric-server: DID operator grants enabled for {did_count} DIDs (audit key {})",
        operator.audit_key_id()
    );
    Ok(Some(operator))
}

/// Build the optional Continuity Watchman state and spawn its background
/// loops. The feature is off unless explicitly enabled, so a plain sidecar
/// makes no PLC export connection.
fn open_continuity() -> Result<Option<Continuity>, Box<dyn std::error::Error>> {
    let settings = parse_continuity_settings(
        env::var("ATMOSPHERIC_SERVER_CONTINUITY_ENABLED").ok(),
        env::var("ATMOSPHERIC_SERVER_CONTINUITY_PLC_DIRECTORY").ok(),
        env::var("ATMOSPHERIC_SERVER_CONTINUITY_POLL_SECONDS").ok(),
        env::var("ATMOSPHERIC_SERVER_CONTINUITY_JITTER_SECONDS").ok(),
    )
    .map_err(std::io::Error::other)?;
    let Some(settings) = settings else {
        eprintln!(
            "atmospheric-server: continuity watchman disabled; set ATMOSPHERIC_SERVER_CONTINUITY_ENABLED=true"
        );
        return Ok(None);
    };
    settings
        .validate()
        .map_err(|error| std::io::Error::other(format!("continuity policy: {error}")))?;
    let continuity = Continuity::with_settings(settings.clone());
    let resolver = atmospheric::oauth::Resolver {
        handle_service: env::var("ATMOSPHERIC_SERVER_CONTINUITY_HANDLE_SERVICE")
            .unwrap_or_else(|_| "https://bsky.social".into()),
        plc_directory: settings.plc_directory.clone(),
    };
    let watcher = Watcher::new(continuity.clone(), settings.clone(), resolver);
    tokio::spawn(async move {
        if let Err(error) = watcher.run().await {
            eprintln!("atmospheric-server: continuity watcher stopped: {error}");
        }
    });
    println!(
        "atmospheric-server: continuity watchman enabled via {} (poll {}s, jitter {}s)",
        settings.plc_directory, settings.poll_secs, settings.jitter_secs
    );
    Ok(Some(continuity))
}

/// Build the optional Ledger of Decisions. Off by default: an operator opts in
/// with both a flag and the server DID that will issue the signed records, so a
/// plain sidecar creates neither a signing key nor a subject pepper.
fn open_ledger(data_dir: &Path) -> Result<Option<DecisionLedger>, Box<dyn std::error::Error>> {
    let settings = parse_ledger_settings(
        env::var("ATMOSPHERIC_SERVER_LEDGER_ENABLED").ok(),
        env::var("ATMOSPHERIC_SERVER_LEDGER_ISSUER_DID").ok(),
    )
    .map_err(std::io::Error::other)?;
    let Some(settings) = settings else {
        eprintln!(
            "atmospheric-server: decision ledger disabled; set ATMOSPHERIC_SERVER_LEDGER_ENABLED=true and ATMOSPHERIC_SERVER_LEDGER_ISSUER_DID"
        );
        return Ok(None);
    };
    let ledger = DecisionLedger::open(data_dir, &settings)?;
    println!(
        "atmospheric-server: decision ledger enabled as {} (signing key {})",
        settings.issuer,
        ledger.key_id()
    );
    Ok(Some(ledger))
}

/// Parse the ledger policy from already-read environment values. Pure so startup
/// validation can be tested without mutating the process environment.
fn parse_ledger_settings(
    enabled: Option<String>,
    issuer: Option<String>,
) -> Result<Option<DecisionSettings>, String> {
    let enabled = enabled
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    if !enabled {
        return Ok(None);
    }
    let issuer = issuer
        .map(|raw| raw.trim().to_owned())
        .filter(|raw| !raw.is_empty())
        .ok_or_else(|| "ledger issuer DID is required when the ledger is enabled".to_owned())?;
    Ok(Some(DecisionSettings::new(&issuer)?))
}

/// Parse the watcher policy from already-read environment values so startup
/// validation is testable without mutating the process environment.
fn parse_continuity_settings(
    enabled: Option<String>,
    directory: Option<String>,
    poll: Option<String>,
    jitter: Option<String>,
) -> Result<Option<ContinuitySettings>, String> {
    let enabled = enabled
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    if !enabled {
        return Ok(None);
    }
    let number = |raw: Option<String>, name: &str, default: u64| -> Result<u64, String> {
        match raw {
            Some(raw) => raw
                .trim()
                .parse::<u64>()
                .map_err(|error| format!("invalid {name} {raw:?}: {error}")),
            None => Ok(default),
        }
    };
    let settings = ContinuitySettings {
        plc_directory: directory
            .map(|raw| raw.trim().trim_end_matches('/').to_owned())
            .filter(|raw| !raw.is_empty())
            .unwrap_or_else(|| "https://plc.directory".into()),
        poll_secs: number(
            poll,
            "continuity poll seconds",
            ContinuitySettings::default().poll_secs,
        )?,
        jitter_secs: number(
            jitter,
            "continuity jitter seconds",
            ContinuitySettings::default().jitter_secs,
        )?,
    };
    settings.validate()?;
    Ok(Some(settings))
}

/// Parse the operator policy from already-read environment values. Keeping
/// this pure lets startup validation be tested without mutating the process
/// environment or touching the audit key.
fn parse_operator_settings(
    dids: Option<String>,
    grant_ttl: Option<String>,
    scopes: Option<String>,
) -> Result<Option<OperatorSettings>, String> {
    let dids = csv(dids);
    if dids.is_empty() {
        return Ok(None);
    }
    let grant_ttl_secs = match grant_ttl {
        Some(raw) => raw
            .trim()
            .parse::<u64>()
            .map_err(|error| format!("invalid operator grant lifetime {raw:?}: {error}"))?,
        None => DEFAULT_GRANT_TTL_SECS,
    };
    let scopes = match scopes {
        Some(raw) => csv(Some(raw))
            .iter()
            .map(|scope| GrantScope::from_str(scope))
            .collect::<Result<Vec<_>, _>>()?,
        None => GrantScope::all(),
    };
    let settings = OperatorSettings {
        dids,
        grant_ttl_secs,
        scopes,
    };
    settings.validate()?;
    Ok(Some(settings))
}

fn csv(raw: Option<String>) -> Vec<String> {
    raw.map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_allowlist_leaves_operator_grants_off() {
        assert_eq!(parse_operator_settings(None, None, None).unwrap(), None);
        assert_eq!(
            parse_operator_settings(Some(" , , ".into()), None, None).unwrap(),
            None
        );
    }

    #[test]
    fn operator_policy_parses_and_defaults() {
        let settings = parse_operator_settings(
            Some(" did:plc:one , did:web:two.example ".into()),
            Some(" 120 ".into()),
            Some("links_read, audit_read".into()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(settings.dids, ["did:plc:one", "did:web:two.example"]);
        assert_eq!(settings.grant_ttl_secs, 120);
        assert_eq!(
            settings.scopes,
            [GrantScope::LinksRead, GrantScope::AuditRead]
        );

        let defaults = parse_operator_settings(Some("did:plc:one".into()), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(defaults.grant_ttl_secs, DEFAULT_GRANT_TTL_SECS);
        assert_eq!(defaults.scopes, GrantScope::all());
    }

    #[test]
    fn ledger_policy_defaults_and_validation() {
        assert_eq!(parse_ledger_settings(None, None).unwrap(), None);
        assert_eq!(
            parse_ledger_settings(Some(" off ".into()), None).unwrap(),
            None
        );
        assert!(parse_ledger_settings(Some("true".into()), None).is_err());
        assert!(parse_ledger_settings(Some("1".into()), Some("@alice".into())).is_err());
        let settings = parse_ledger_settings(Some("yes".into()), Some(" did:plc:server ".into()))
            .unwrap()
            .unwrap();
        assert_eq!(settings.issuer, "did:plc:server");
    }

    #[test]
    fn continuity_policy_defaults_and_validation() {
        assert_eq!(
            parse_continuity_settings(None, None, None, None).unwrap(),
            None
        );
        assert_eq!(
            parse_continuity_settings(Some(" off ".into()), None, None, None).unwrap(),
            None
        );
        let settings = parse_continuity_settings(
            Some("true".into()),
            Some(" https://plc.example/ ".into()),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(settings.plc_directory, "https://plc.example");
        assert_eq!(settings.poll_secs, 300);
        assert_eq!(settings.jitter_secs, 30);

        assert!(
            parse_continuity_settings(Some("yes".into()), None, Some("29".into()), None).is_err()
        );
        assert!(
            parse_continuity_settings(Some("1".into()), None, None, Some("301".into())).is_err()
        );
        assert!(
            parse_continuity_settings(Some("1".into()), Some("plc.example".into()), None, None)
                .is_err()
        );
    }

    #[test]
    fn invalid_operator_policy_is_rejected() {
        assert!(parse_operator_settings(Some("did:key:no".into()), None, None).is_err());
        assert!(
            parse_operator_settings(Some("did:plc:one".into()), Some("0".into()), None).is_err()
        );
        assert!(
            parse_operator_settings(
                Some("did:plc:one".into()),
                None,
                Some("links_read,unknown".into())
            )
            .is_err()
        );
        assert!(
            parse_operator_settings(Some("did:plc:one".into()), None, Some("".into())).is_err()
        );
    }
}
