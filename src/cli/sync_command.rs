//! One original command budget across connected Sync or its initial direct fallback.
use crate::config::Config;
use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;
use crate::ipc::async_client::{AsyncClient, within_operation};
use crate::search::operation::SearchOperation;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

enum Outcome {
    Daemon(Value),
    Direct(Value),
}
pub(super) fn run(
    path: Option<&Path>,
    settings: &crate::config::schema::Resolved<crate::store::settings::DbSettings>,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let global = Config::global()?;
    let path = path.map(Path::to_path_buf);
    let settings = settings.clone();
    let outcome = super::command::run(global, move |context| {
        run_in(context, path, settings, started)
    })?;
    match outcome {
        Outcome::Direct(value) => println!("{value}"),
        Outcome::Daemon(value) => {
            println!("{}", serde_json::to_string_pretty(&value)?);
            if value["state"] == "error" {
                anyhow::bail!("sync job {} failed: {}", value["job_id"], value["stats"]);
            }
        }
    }
    Ok(())
}
async fn run_in(
    context: super::command::Context,
    path: Option<PathBuf>,
    settings: crate::config::schema::Resolved<crate::store::settings::DbSettings>,
    started: Instant,
) -> Result<Outcome> {
    let operation = SearchOperation::with_timeout(
        started,
        Duration::from_millis(context.global.remote.background_timeout_ms),
        Cancellation::new(),
    )?;
    let mut cancellation =
        crate::daemon::request::CancelOnDrop(Some(operation.cancellation().clone()));
    let activity = context.activity.track();
    let target = context
        .control(&operation, &activity, move || {
            let cwd = std::env::current_dir()
                .map_err(|e| Error::internal_with_source("resolve current directory", e))?;
            let target = cwd.join(path.as_deref().unwrap_or(Path::new(".")));
            target.canonicalize().map_err(|e| {
                Error::internal_with_source(format!("resolve sync target {}", target.display()), e)
            })
        })
        .await?;
    let outcome = within_operation(&operation, async {
        let connected = AsyncClient::connect(&context.cache).await;
        if let Ok(mut client) = connected {
            client.handshake_registry().await?;
            let website = client
                .capabilities()
                .iter()
                .any(|cap| cap == crate::ipc::protocol::ACTIVE_WEBSITE_CAPABILITY);
            let binding = if website {
                let cache = context.cache.clone();
                let root = target.clone();
                let settings = settings.clone();
                let project = context
                    .control(&operation, &activity, move || {
                        super::existing_website_project(&cache, &root, &settings)
                    })
                    .await?;
                bind_website(&mut client, project.id).await
            } else {
                client.register_session(&target).await
            };
            match binding {
                Ok(()) => {
                    let mut job = client.call_tool("sync_start", json!({})).await?;
                    let id = job["job_id"].as_i64().ok_or_else(|| Error::Protocol {
                        message: "sync_start: missing job_id".into(),
                    })?;
                    while matches!(job["state"].as_str(), Some("queued" | "running")) {
                        operation.check()?;
                        tokio::time::sleep(Duration::from_millis(
                            context.global.daemon.poll_interval_ms,
                        ))
                        .await;
                        job = client
                            .call_tool("sync_status", json!({"job_id":id}))
                            .await?;
                    }
                    return Ok(Outcome::Daemon(job));
                }
                Err(Error::Project { .. }) if !website => {}
                Err(error) => return Err(error),
            }
            // Semantic unindexed-project binding permits the existing direct
            // bootstrap; connected transport/tool failures above never replay.
            drop(client);
        } else {
            operation.check()?;
            if matches!(
                context.global.daemon.profile,
                crate::daemon::settings::DaemonProfile::Website { .. }
            ) {
                return Err(crate::daemon::launch::website_unavailable());
            }
        }
        super::index::run_with_operation(
            context.clone(),
            Some(target),
            settings,
            operation.clone(),
            activity.clone(),
        )
        .await
        .map(Outcome::Direct)
    })
    .await;
    if outcome.is_ok() {
        cancellation.0 = None;
    }
    outcome
}

async fn bind_website(client: &mut AsyncClient, project: i64) -> Result<()> {
    // Active Website is the existing Sync binding authority; panel-read-v1
    // negotiation belongs to the panel API and was never a Sync prerequisite.
    use crate::ipc::protocol::{Request, Response};
    match client
        .call(Request::BindReadOnly {
            pid: std::process::id(),
            project_id: Some(project),
        })
        .await?
    {
        Response::ToolResult { .. } => Ok(()),
        Response::Error {
            code,
            message,
            instruction,
        } => Err(Error::from_mcp_code(code, message, instruction)),
        other @ Response::Hello { .. } => Err(Error::Protocol {
            message: format!("unexpected binding reply: {other:?}"),
        }),
    }
}
