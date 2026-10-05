//! Startup wiring: configuration, logging, the HTTP server, and graceful shutdown.

use std::process::ExitCode;

use rustle::{AppState, Config, build_router};
use tokio::net::TcpListener;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    // Fills in a variable only if the real process environment does not already have it,
    // and does nothing at all if there is no `.env` file — so this is a local-development
    // convenience, never a second source of truth in production.
    dotenvy::dotenv().ok();

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(err) => {
            // Logging is not up yet, and a misconfigured instance should say so on stderr
            // rather than in a structured log nobody is reading.
            eprintln!("rustle: configuration error: {err}");
            eprintln!("rustle: see .env.example for the supported variables");
            return ExitCode::FAILURE;
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(format!("rustle={}", config.log_level))),
        )
        .init();

    match serve(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let listen_addr = config.listen_addr;

    let db = rustle::db::connect(&config.database_url).await?;
    // Migrations run on every start, so an upgrade is just "replace the binary".
    rustle::db::migrate(&db).await?;
    tracing::info!("database schema up to date");

    let state = AppState::new(config, db);

    // Printed only while there is nobody to log in as. This is what stops a stranger who
    // finds a fresh public instance from claiming it before its owner reaches it.
    if !rustle::db::user::exists(state.db()).await? {
        tracing::warn!(
            "no user yet. Open {}/setup and enter this one-time setup token: {}",
            state.config().base_url.as_str().trim_end_matches('/'),
            state.setup_token(),
        );
    }

    let listener = TcpListener::bind(listen_addr).await?;
    tracing::info!(
        "rustle {} listening on http://{}",
        env!("CARGO_PKG_VERSION"),
        listener.local_addr()?
    );

    let poller_shutdown = CancellationToken::new();
    let poller = tokio::spawn(rustle::feed::scheduler::run(
        state.clone(),
        poller_shutdown.clone(),
    ));

    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // The HTTP server only stops once every in-flight request has finished; the poller is
    // told to stop at the same moment and is then awaited, so a shutdown cannot land
    // mid-fetch.
    poller_shutdown.cancel();
    if let Err(err) = poller.await {
        tracing::error!("poller task panicked: {err}");
    }

    Ok(())
}

/// Resolves on Ctrl-C, or on SIGTERM when systemd or Docker stops the service.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(err) => tracing::warn!("cannot listen for SIGTERM: {err}"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }

    tracing::info!("shutting down");
}
