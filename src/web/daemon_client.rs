//! Bounded reusable IPC leases. An interrupted exchange destroys its connection.
use super::settings::WebSettings;
use crate::{
    error::{Error, Result},
    hybrid::work::Cancellation,
    ipc::{async_client::AsyncClient, panel::PanelInfo},
    search::{operation::SearchOperation, settings::SearchSettings},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

struct Connection {
    client: AsyncClient,
    project: Option<i64>,
}
struct Pool {
    idle: Mutex<Vec<Connection>>,
    active: Arc<Semaphore>,
    admitted: Arc<Semaphore>,
}
impl Pool {
    fn new(count: usize, queue: usize) -> Self {
        Self {
            idle: Mutex::new(Vec::new()),
            active: Arc::new(Semaphore::new(count)),
            admitted: Arc::new(Semaphore::new(count + queue)),
        }
    }
}
pub struct DaemonClient {
    cache: PathBuf,
    search: Pool,
    control: Pool,
    pub info: PanelInfo,
    settings: WebSettings,
}
impl DaemonClient {
    pub async fn connect(cache: PathBuf, settings: WebSettings) -> Result<Self> {
        let mut client = Self::connection(&cache).await?;
        let info = serde_json::from_value(client.client.call_tool("panel_info", json!({})).await?)
            .map_err(|e| Error::internal_with_source("invalid panel info", e))?;
        let this = Self {
            cache,
            search: Pool::new(settings.search_connections, settings.queue_limit),
            control: Pool::new(settings.control_connections, settings.queue_limit),
            info,
            settings,
        };
        this.control
            .idle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(client);
        for _ in 1..this.settings.control_connections {
            let conn = Self::connection(&this.cache).await?;
            this.control
                .idle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(conn);
        }
        Ok(this)
    }
    async fn connection(cache: &std::path::Path) -> Result<Connection> {
        let mut client = AsyncClient::connect(cache)
            .await
            .map_err(|e| Error::transport_with_source("daemon unavailable", e))?;
        client.handshake_registry().await?;
        client.ensure_panel_read()?;
        if !client
            .capabilities()
            .iter()
            .any(|capability| capability == crate::ipc::protocol::ACTIVE_WEBSITE_CAPABILITY)
        {
            client.bind_read_only(None).await?;
        }
        Ok(Connection {
            client,
            project: None,
        })
    }
    pub async fn call(
        &self,
        project: Option<i64>,
        tool: &str,
        args: Value,
        started: Instant,
    ) -> Result<Value> {
        let searching = tool == "search_docs";
        let pool = if searching {
            &self.search
        } else {
            &self.control
        };
        let _admitted = Arc::clone(&pool.admitted)
            .try_acquire_owned()
            .map_err(|_| Error::Admission {
                message: "web busy".into(),
            })?;
        let policy = SearchSettings {
            request_timeout_ms: self.info.request_timeout_ms,
            dedupe_max_candidates: self.info.dedupe_max_candidates,
            max_request_payload_bytes: self.info.max_request_payload_bytes,
            max_retained_payload_bytes: self.info.max_retained_payload_bytes,
        };
        let operation = SearchOperation::new(&policy, started, Cancellation::new())?;
        let deadline = if searching {
            operation.deadline()
        } else {
            started + Duration::from_millis(self.settings.control_timeout_ms)
        };
        let work = async {
            let _active = pool
                .active
                .acquire()
                .await
                .map_err(|_| Error::transport("pool closed"))?;
            operation.record_queue(started.elapsed());
            let cached = pool
                .idle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop();
            let mut conn = match cached {
                Some(conn) => conn,
                None => Self::connection(&self.cache).await?,
            };
            // Website registry reads are already filtered by the backend. Keep
            // the actual binding when a pooled connection was previously bound;
            // project-required tools still go through explicit ID/null admission.
            let registry_read = project.is_none()
                && matches!(
                    tool,
                    "panel_info" | "panel_projects" | "list_projects" | "status"
                )
                && conn.client.capabilities().iter().any(|capability| {
                    capability == crate::ipc::protocol::ACTIVE_WEBSITE_CAPABILITY
                });
            if conn.project != project && !registry_read {
                conn.client.bind_read_only(project).await?;
                conn.project = project;
            }
            let result = if searching {
                conn.client.search(args, &operation).await
            } else {
                conn.client.call_tool(tool, args).await
            };
            // Failed, cancelled or partially read connections are never reused.
            if result.is_ok() {
                pool.idle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(conn);
            }
            result
        };
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), work)
            .await
            .map_err(|_| Error::Admission {
                message: "web deadline expired".into(),
            })?
    }
}
