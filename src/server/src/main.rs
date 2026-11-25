use anyhow::Context;
use clap::{Parser, Subcommand};
use snapdock_server::{AppState, create_user, open_database, router, set_setting};
use std::{net::SocketAddr, path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(version, about = "SnapDock - HTTPS Encrypted Settings Sync Server")]
struct Cli {
    #[arg(
        long,
        global = true,
        env = "SCREENSHOT_DATA_DIR",
        default_value = "data"
    )]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Initialize the database and local development TLS certificate
    Init,
    /// Start the HTTPS server
    Serve {
        #[arg(long, env = "SCREENSHOT_BIND", default_value = "127.0.0.1:8899")]
        bind: SocketAddr,
        #[arg(long, env = "SCREENSHOT_TLS_CERT")]
        cert: Option<PathBuf>,
        #[arg(long, env = "SCREENSHOT_TLS_KEY")]
        key: Option<PathBuf>,
    },
    /// View or update runtime settings in the database (no restart required)
    Settings {
        key: Option<String>,
        value: Option<String>,
    },
    /// Create a user (password read interactively or via SCREENSHOT_ADMIN_PASSWORD)
    CreateUser { username: String },
}
fn ensure_certificate(dir: &std::path::Path) -> anyhow::Result<()> {
    let cert = dir.join("server.crt");
    let key = dir.join("server.key");
    if cert.exists() && key.exists() {
        return Ok(());
    }
    anyhow::ensure!(
        !cert.exists() && !key.exists(),
        "Incomplete TLS certificate/key pair; please provide both valid certificate and private key"
    );
    let rcgen::CertifiedKey {
        cert: certificate,
        key_pair,
    } = rcgen::generate_simple_self_signed(vec![
        "localhost".into(),
        "127.0.0.1".into(),
        "::1".into(),
    ])?;
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&key)?
        .write_all(key_pair.serialize_pem().as_bytes())?;
    std::fs::write(cert, certificate.pem())?;
    Ok(())
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "snapdock_server=info,axum_server=info".into()),
        )
        .init();
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.data_dir)?;
    let db = open_database(cli.data_dir.join("snapdock.sqlite3"))?;
    match cli.command.unwrap_or(Command::Serve {
        bind: "127.0.0.1:8899".parse()?,
        cert: None,
        key: None,
    }) {
        Command::Init => {
            ensure_certificate(&cli.data_dir)?;
            println!(
                "Initialized: {}",
                std::fs::canonicalize(&cli.data_dir)?.display()
            );
        }
        Command::Settings { key, value } => match (key, value) {
            (Some(key), Some(value)) => {
                set_setting(&db, &key, &value)?;
                println!("{key}={value}");
            }
            (None, None) => {
                let mut statement =
                    db.prepare("SELECT key,value FROM server_settings ORDER BY key")?;
                for row in statement
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                {
                    let (k, v) = row?;
                    println!("{k}={v}");
                }
            }
            _ => anyhow::bail!("Please provide both setting key and value"),
        },
        Command::CreateUser { username } => {
            let password = std::env::var("SCREENSHOT_ADMIN_PASSWORD").unwrap_or_else(|_| {
                rpassword::prompt_password("Password (at least 10 characters): ").unwrap_or_default()
            });
            create_user(&db, &username, &password)?;
            println!("User {username} created");
        }
        Command::Serve { bind, cert, key } => {
            anyhow::ensure!(
                cert.is_some() == key.is_some(),
                "--cert and --key must both be provided"
            );
            let (cert, key) = match (cert, key) {
                (Some(cert), Some(key)) => (cert, key),
                _ => {
                    ensure_certificate(&cli.data_dir)?;
                    (
                        cli.data_dir.join("server.crt"),
                        cli.data_dir.join("server.key"),
                    )
                }
            };
            rustls::crypto::ring::default_provider()
                .install_default()
                .map_err(|_| anyhow::anyhow!("TLS provider initialization failed"))?;
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                .await
                .context("Failed to read TLS certificate or private key")?;
            let handle = axum_server::Handle::new();
            let stop = handle.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    stop.graceful_shutdown(Some(Duration::from_secs(10)));
                }
            });
            tracing::info!("Settings sync server listening on https://{bind}");
            tracing::info!(certificate=%cert.display(), "Local clients can import this public certificate");
            axum_server::bind_rustls(bind, tls)
                .handle(handle)
                .serve(
                    router(AppState::new(db)?).into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await?;
        }
    }
    Ok(())
}
