//! Embedded local Web frontend, backed exclusively by daemon IPC.
pub mod contracts;
mod daemon_client;
mod server;
pub mod settings;
mod assets {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}
use anyhow::Context;
use std::fmt::Write as _;
use std::{
    sync::{Arc, Mutex, atomic::AtomicU64},
    time::{Duration, Instant},
};

fn token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("OS entropy unavailable"))?;
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}")?;
    }
    Ok(encoded)
}
/// Run until Ctrl+C. The shared daemon outlives this frontend.
/// # Errors
/// Invalid settings, unavailable/incompatible daemon, bind or HTTP errors.
pub fn run(port: Option<u16>, open: bool) -> anyhow::Result<()> {
    let settings = settings::load(port)?.into_settings();
    let runtime = tokio::runtime::Runtime::new()?;
    let shutdown_ms = settings.shutdown_timeout_ms;
    let result = runtime.block_on(async move {
        let listener = crate::platform::web::bind_loopback(settings.port)
            .await
            .context("cannot bind local Web port (it may already be in use)")?;
        let address = listener.local_addr()?;
        let cache = crate::config::paths::cache_dir()?;
        let startup_cache = cache.clone();
        let startup_deadline =
            tokio::time::Instant::now() + Duration::from_millis(settings.startup_timeout_ms);
        tokio::time::timeout_at(
            startup_deadline,
            tokio::task::spawn_blocking(move || {
                crate::daemon::lifecycle::ensure_daemon(&startup_cache)
            }),
        )
        .await
        .context("daemon startup deadline")???;
        let daemon = tokio::time::timeout_at(
            startup_deadline,
            daemon_client::DaemonClient::connect(cache, settings.clone()),
        )
        .await
        .context("daemon handshake deadline")??;
        let projects = tokio::time::timeout_at(
            startup_deadline,
            daemon.call(
                None,
                "panel_projects",
                serde_json::json!({}),
                Instant::now(),
            ),
        )
        .await
        .context("daemon startup metadata deadline")??;
        let projects: Vec<crate::ipc::panel::ProjectView> = serde_json::from_value(projects)?;
        let cwd = std::env::current_dir()?.canonicalize()?;
        let initial_project = projects
            .iter()
            .filter(|p| cwd.starts_with(&p.root))
            .max_by_key(|p| p.root.len())
            .map(|p| p.id);
        let bootstrap = token()?;
        let url = format!("http://{address}/#bootstrap={bootstrap}");
        let state = Arc::new(server::AppState {
            daemon,
            settings,
            authority: address.to_string(),
            bootstrap: Mutex::new(Some(bootstrap)),
            cookie_name: format!("docsbase_{}", token()?),
            cookie_value: token()?,
            initial_project,
            serial: AtomicU64::new(1),
        });
        let serving = axum::serve(listener, server::router(state)).into_future();
        serve_until_shutdown(serving, || {
            println!("docsbase Web: {url}");
            if open && let Err(error) = crate::platform::web::open_browser(&url) {
                eprintln!("Browser could not be opened: {error}. Use the launch URL above.");
            }
        })
        .await?;
        // Dropping the HTTP service cancels its request futures; the runtime
        // shutdown below bounds any blocking startup helpers still finishing.
        Ok::<(), anyhow::Error>(())
    });
    runtime.shutdown_timeout(Duration::from_millis(shutdown_ms));
    result
}

async fn serve_until_shutdown(
    serving: impl Future<Output = std::io::Result<()>>,
    announce: impl FnOnce(),
) -> anyhow::Result<()> {
    // Install synchronously before readiness or browser launch. A ctrl_c()
    // future registers only on its first poll, leaving that boundary unarmed.
    let mut shutdown =
        crate::platform::ShutdownSignal::new().context("cannot install Web shutdown handlers")?;
    announce();
    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => result?,
        () = shutdown.recv() => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
