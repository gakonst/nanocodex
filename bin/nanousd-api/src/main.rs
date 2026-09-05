mod app;
mod config;
mod db;
mod issuer;
mod stripe;

use clap::Parser;
use eyre::{Context, Result};

use crate::{
    app::{AppState, fulfillment_worker, router},
    config::Config,
};

#[tokio::main]
async fn main() -> Result<()> {
    nanousd::install_default_rustls_crypto_provider();
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nanousd_api=info".into()),
        )
        .init();
    let config = Config::parse();
    let bind = config.bind;
    let state = AppState::new(config)?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .wrap_err_with(|| format!("failed to bind NanoUSD API to {bind}"))?;
    let worker = tokio::spawn(fulfillment_worker(state.clone()));
    tracing::info!(%bind, "NanoUSD API listening");
    let result = axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .wrap_err("NanoUSD API server failed");
    worker.abort();
    result
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl-C handler");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::error!(%error, "failed to install termination handler"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
