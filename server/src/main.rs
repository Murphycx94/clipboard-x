//! `clipboardx-sync-server --listen 127.0.0.1:8787 --data /var/lib/clipboardx-sync`
//!
//! Listen on this machine only: a TLS proxy in front of it (Caddy, see
//! `server/deploy`) serves it over HTTPS, so device tokens never cross a
//! network in the clear.

use std::net::SocketAddr;
use std::path::PathBuf;

use clipboardx_sync_server::{Config, issue_pairing_code, new_state, router, store::Store};

const USAGE: &str = "usage: clipboardx-sync-server [--listen ADDR:PORT] [--data DIR]
  --listen   address to listen on (env CLIPBOARDX_SYNC_LISTEN, default 127.0.0.1:8787);
             keep it on this machine, behind an HTTPS proxy
  --data     directory of the database (env CLIPBOARDX_SYNC_DATA, default ./data)

SIGUSR1 prints a pairing code (valid 10 minutes), for when no device is
left to make one: systemctl kill -s USR1 clipboardx-sync";

struct Args {
    listen: SocketAddr,
    data: PathBuf,
}

fn args() -> Result<Args, String> {
    let mut listen =
        std::env::var("CLIPBOARDX_SYNC_LISTEN").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let mut data = std::env::var("CLIPBOARDX_SYNC_DATA").unwrap_or_else(|_| "data".into());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = args.next().ok_or("--listen needs a value")?,
            "--data" => data = args.next().ok_or("--data needs a value")?,
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    Ok(Args {
        listen: listen
            .parse()
            .map_err(|e| format!("--listen {listen}: {e}"))?,
        data: PathBuf::from(data),
    })
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = match args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return std::process::ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&args.data) {
        log::error!("cannot create {}: {e}", args.data.display());
        return std::process::ExitCode::FAILURE;
    }
    let store = match Store::open(&args.data.join("sync.db")) {
        Ok(store) => store,
        Err(e) => {
            log::error!("cannot open the database: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (state, admin_code) = match new_state(store, Config::default()) {
        Ok(state) => state,
        Err(e) => {
            log::error!("cannot read the database: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Some(code) = admin_code {
        log::warn!("no device yet: admin code {code} (valid 10 minutes; restart for a new one)");
    }
    #[cfg(unix)]
    {
        let state = state.clone();
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()) {
            Ok(mut signals) => {
                tokio::spawn(async move {
                    while signals.recv().await.is_some() {
                        let code = issue_pairing_code(&state);
                        log::warn!("pairing code {} (valid 10 minutes)", code.code);
                    }
                });
            }
            Err(e) => log::error!("cannot listen for SIGUSR1: {e}"),
        }
    }
    let listener = match tokio::net::TcpListener::bind(args.listen).await {
        Ok(listener) => listener,
        Err(e) => {
            log::error!("cannot listen on {}: {e}", args.listen);
            return std::process::ExitCode::FAILURE;
        }
    };
    log::info!("listening on {}", args.listen);
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        log::info!("shutting down");
    };
    match axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
    {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log::error!("server error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
