use eyre::{Result, WrapErr};
use nanocodex_exe_dev::{Config, application, build_agent};

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    let (agent, events) = build_agent(&config)?;
    let (app, runtime) = application(agent, events, &config);
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .wrap_err_with(|| format!("failed to listen on {}", config.listen))?;
    eprintln!(
        "nanocodex exe.dev session listening on http://{} for workspace {}",
        config.listen,
        config.workspace.display()
    );

    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    let server_result = server.await;
    let shutdown_result = runtime.shutdown().await;
    server_result.wrap_err("exe.dev HTTP server failed")?;
    shutdown_result
}

async fn shutdown_signal() {
    let interrupt = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
}
