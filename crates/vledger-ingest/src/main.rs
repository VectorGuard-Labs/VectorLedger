//! `vledger-ingest` — CLI binary for the VectorLedger ingestion pipeline.
//!
//! # Usage
//!
//! ```bash
//! # Interactive OAuth flow (opens browser)
//! vledger-ingest \
//!   --data-dir ./vledger-data \
//!   --source banno \
//!   --audit-package ./audit-$(date +%Y%m%d).json
//!
//! # Inject a pre-existing token (for scripted / CI use)
//! vledger-ingest \
//!   --data-dir ./vledger-data \
//!   --source banno \
//!   --token <access_token> \
//!   --user-id 75b6830d-d6ab-4e89-93d6-a9d9ac77503c
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use vledger_banno_connector::{BannoConfig, BannoConnector};
use vledger_banking_core::CoreBankingConnector;
use vledger_ingest::IngestPipeline;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Parser)]
#[command(
    name = "vledger-ingest",
    about = "Ingest banking data from a connector into VectorLedger",
    version
)]
struct Args {
    /// VectorLedger data directory.
    #[arg(short = 'd', long, default_value = "./vledger-data")]
    data_dir: PathBuf,

    /// Banking source connector to use.
    #[arg(short = 's', long, default_value = "banno")]
    source: String,

    /// Pre-existing OAuth access token (skips interactive auth).
    /// Requires --user-id to also be set.
    #[arg(long)]
    token: Option<String>,

    /// User ID associated with the access token (required with --token).
    #[arg(long)]
    user_id: Option<String>,

    /// Path to write the cryptographic audit package.
    /// Defaults to ./vledger-audit-package-<timestamp>.json
    #[arg(short = 'o', long)]
    audit_package: Option<PathBuf>,

    /// VectorLedger domain for ingested entries.
    #[arg(long, default_value = "banno-garden")]
    domain: String,

    /// Log level (trace, debug, info, warn, error).
    #[arg(short = 'l', long, default_value = "info")]
    log_level: String,
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialise tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&args.log_level)),
        )
        .init();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        data_dir = %args.data_dir.display(),
        source = %args.source,
        "vledger-ingest starting"
    );

    // ── Initialise the data directory if it doesn't exist ─────────────────
    let data_dir = &args.data_dir;
    std::fs::create_dir_all(data_dir.join("wal"))
        .context("Failed to create WAL directory")?;
    std::fs::create_dir_all(data_dir.join("pages"))
        .context("Failed to create pages directory")?;
    std::fs::create_dir_all(data_dir.join("audit"))
        .context("Failed to create audit directory")?;

    // ── Audit package path ─────────────────────────────────────────────────
    let audit_package_path = args.audit_package.unwrap_or_else(|| {
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        PathBuf::from(format!("vledger-audit-package-{ts}.json"))
    });

    // ── Build and authenticate the connector ──────────────────────────────
    match args.source.to_lowercase().as_str() {
        "banno" => {
            run_banno(
                data_dir,
                &audit_package_path,
                &args.domain,
                args.token.as_deref(),
                args.user_id.as_deref(),
            )
            .await?;
        }
        other => {
            anyhow::bail!("Unknown source connector: '{other}'. Supported: banno");
        }
    }

    Ok(())
}

async fn run_banno(
    data_dir: &PathBuf,
    audit_package_path: &PathBuf,
    domain: &str,
    token: Option<&str>,
    user_id: Option<&str>,
) -> Result<()> {
    let config = BannoConfig::from_env()
        .map_err(|e| anyhow::anyhow!("Failed to load Banno config from environment.\n\
                  Required: BANNO_CLIENT_ID, BANNO_CLIENT_SECRET\n\
                  Optional: BANNO_REDIRECT_URI, BANNO_BASE_URL, BANNO_REDIRECT_PORT\n\
                  Error: {e}"))?;

    let mut connector = BannoConnector::new(config);

    // Either inject a pre-existing token or perform interactive OAuth flow
    if let (Some(tok), Some(uid)) = (token, user_id) {
        info!("Using pre-existing access token");
        connector = connector.with_token(tok, uid);
    } else {
        info!("Starting interactive OAuth authentication...");
        connector
            .authenticate()
            .await
            .context("Authentication failed")?;
        info!("Authentication successful");
    }

    // ── Run the pipeline ──────────────────────────────────────────────────
    let pipeline = IngestPipeline::new(data_dir)
        .with_audit_package(audit_package_path)
        .with_domain(domain);

    let report = pipeline
        .run(&connector)
        .await
        .context("Ingestion pipeline failed")?;

    // ── Print the report ──────────────────────────────────────────────────
    report.print_summary();

    // Save the report as JSON
    let report_path = data_dir.join("last-ingest-report.json");
    let report_json = serde_json::to_string_pretty(&report)
        .context("Failed to serialize report")?;
    std::fs::write(&report_path, report_json)
        .context("Failed to write report")?;
    info!(path = %report_path.display(), "Report saved");

    // Exit with non-zero if chain verification failed
    if !report.chain_verified {
        anyhow::bail!("Hash chain verification failed — see logs for details");
    }

    Ok(())
}
