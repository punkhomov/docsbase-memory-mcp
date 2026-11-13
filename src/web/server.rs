//! Local HTTP security boundary. No raw daemon diagnostics cross it.
use super::{
    contracts::{ApiError, SearchRequest, SessionView},
    daemon_client::DaemonClient,
    settings::WebSettings,
};
use crate::{error::Error, ipc::panel::ProjectView};
use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Bootstrap {
    token: String,
}

pub struct AppState {
    pub daemon: DaemonClient,
    pub settings: WebSettings,
    pub authority: String,
    pub bootstrap: Mutex<Option<String>>,
    pub cookie_name: String,
    pub cookie_value: String,
    pub initial_project: Option<i64>,
    pub serial: AtomicU64,
}
pub fn router(state: Arc<AppState>) -> Router {
    Router::new().fallback(handle).with_state(state)
}
fn failure(status: StatusCode, code: &str, message: &str, id: &str) -> Response {
    (
        status,
        axum::Json(ApiError {
            code: code.into(),
            message: message.into(),
            request_id: id.into(),
        }),
    )
        .into_response()
}
fn mapped(error: &Error, id: &str) -> Response {
    let (status, code, message) = match error {
        Error::Query { .. } => (
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid search query or filters",
        ),
        Error::Project { .. } => (
            StatusCode::NOT_FOUND,
            "unknown_project",
            "Project is unavailable or not indexed",
        ),
        Error::Transport { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "daemon_unavailable",
            "Daemon unavailable; retry the operation explicitly",
        ),
        Error::Protocol { message }
            if message.contains("unsupported") || message.contains("advertise") =>
        {
            (
                StatusCode::NOT_IMPLEMENTED,
                "unsupported",
                "Daemon capability unavailable; restart the daemon",
            )
        }
        Error::Protocol { .. } => (
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid request or incompatible daemon response",
        ),
        Error::Admission { message } if message.contains("deadline") => (
            StatusCode::GATEWAY_TIMEOUT,
            "deadline",
            "Request deadline expired; search was not retried",
        ),
        Error::Admission { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "Request refused; service busy or project not ready",
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Operation failed; inspect local daemon diagnostics",
        ),
    };
    failure(status, code, message, id)
}
async fn handle(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let started = Instant::now();
    let id = state.serial.fetch_add(1, Ordering::Relaxed).to_string();
    let search = request.uri().path().ends_with("/search");
    let budget_ms = if search {
        state.daemon.info.request_timeout_ms
    } else {
        state.settings.control_timeout_ms
    };
    let deadline =
        tokio::time::Instant::from_std(started + std::time::Duration::from_millis(budget_ms));
    let mut response =
        match tokio::time::timeout_at(deadline, dispatch(&state, request, &id, started)).await {
            Ok(response) => response,
            Err(_) => failure(
                StatusCode::GATEWAY_TIMEOUT,
                "deadline",
                "Request deadline expired; search was not retried",
                &id,
            ),
        };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("content-security-policy", HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    if let Ok(value) = HeaderValue::from_str(&id) {
        headers.insert("x-request-id", value);
    }
    eprintln!(
        "web request={id} status={} elapsed_ms={}",
        response.status().as_u16(),
        started.elapsed().as_millis()
    );
    response
}
#[allow(clippy::too_many_lines)] // One audited security boundary before route dispatch.
async fn dispatch(state: &AppState, request: Request, id: &str, started: Instant) -> Response {
    let (parts, body) = request.into_parts();
    let origin = format!("http://{}", state.authority);
    if parts
        .headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        != Some(state.authority.as_str())
        || parts
            .headers
            .get(header::ORIGIN)
            .is_some_and(|v| v.to_str().ok() != Some(origin.as_str()))
    {
        return failure(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Host or Origin rejected",
            id,
        );
    }
    if parts.uri.query().is_some() {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Query strings are not accepted",
            id,
        );
    }
    let path = parts.uri.path();
    let operation = path
        .rsplit('/')
        .next()
        .filter(|name| ["session", "projects", "status", "search", "config"].contains(name))
        .unwrap_or("asset");
    eprintln!("web request={id} operation={operation}");
    let method = parts.method.as_str();
    if !path.starts_with("/api/") {
        if method != "GET" {
            return failure(
                StatusCode::METHOD_NOT_ALLOWED,
                "invalid_request",
                "GET required",
                id,
            );
        }
        return match super::assets::get(path) {
            Some((mime, bytes)) => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
            None => failure(StatusCode::NOT_FOUND, "not_found", "Resource not found", id),
        };
    }
    if method == "POST"
        && (parts
            .headers
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            != Some(origin.as_str())
            || parts
                .headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(';').next())
                != Some(Some("application/json")))
    {
        return failure(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Same-origin JSON required",
            id,
        );
    }
    if path == "/api/v1/session" && method == "POST" {
        let Ok(bytes) = to_bytes(body, state.settings.max_body_bytes).await else {
            return failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                "invalid_request",
                "Request body too large",
                id,
            );
        };
        let Ok(input) = serde_json::from_slice::<Bootstrap>(&bytes) else {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Invalid session request",
                id,
            );
        };
        let mut bootstrap = state
            .bootstrap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if bootstrap.as_ref() != Some(&input.token) {
            return failure(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Bootstrap token invalid or already used",
                id,
            );
        }
        *bootstrap = None;
        let mut response = axum::Json(json!({"authenticated": true})).into_response();
        if let Ok(cookie) = HeaderValue::from_str(&format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/",
            state.cookie_name, state.cookie_value
        )) {
            response.headers_mut().insert(header::SET_COOKIE, cookie);
        }
        return response;
    }
    let expected = format!("{}={}", state.cookie_name, state.cookie_value);
    let authenticated = parts
        .headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .any(|v| v.trim() == expected);
    if !authenticated {
        return failure(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Open the launch URL from the docsbase terminal",
            id,
        );
    }
    if path == "/api/v1/session" && method == "GET" {
        let result = state
            .daemon
            .call(None, "panel_info", json!({}), started)
            .await;
        return match result.and_then(|value| {
            serde_json::from_value(value).map_err(|_| Error::internal("invalid daemon info"))
        }) {
            Ok(daemon) => axum::Json(SessionView {
                version: env!("CARGO_PKG_VERSION").into(),
                poll_interval_ms: state.settings.poll_interval_ms,
                initial_project: state.initial_project,
                daemon,
            })
            .into_response(),
            Err(error) => mapped(&error, id),
        };
    }
    if path == "/api/v1/projects" && method == "GET" {
        return reply(
            state
                .daemon
                .call(None, "panel_projects", json!({}), started)
                .await,
            id,
        );
    }
    let segments: Vec<_> = path.split('/').collect();
    if segments.len() != 6 || segments[1..4] != ["api", "v1", "projects"] {
        return failure(StatusCode::NOT_FOUND, "not_found", "Route not found", id);
    }
    let Ok(project) = segments[4].parse::<i64>() else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid project ID",
            id,
        );
    };
    match (method, segments[5]) {
        ("GET", "config") => reply(
            state
                .daemon
                .call(Some(project), "panel_config", json!({}), started)
                .await,
            id,
        ),
        ("GET", "status") => {
            let result = state
                .daemon
                .call(None, "panel_projects", json!({}), started)
                .await
                .and_then(|value| {
                    let projects: Vec<ProjectView> = serde_json::from_value(value)
                        .map_err(|_| Error::internal("invalid project list"))?;
                    projects
                        .into_iter()
                        .find(|p| p.id == project)
                        .map(|p| json!(p))
                        .ok_or_else(|| Error::Project {
                            message: "unknown project".into(),
                            instruction: None,
                        })
                });
            reply(result, id)
        }
        ("POST", "search") => {
            let Ok(bytes) = to_bytes(body, state.settings.max_body_bytes).await else {
                return failure(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "invalid_request",
                    "Request body too large",
                    id,
                );
            };
            let Ok(input) = serde_json::from_slice::<SearchRequest>(&bytes) else {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "Invalid search request",
                    id,
                );
            };
            if input.query.trim().is_empty() {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "Enter a search query",
                    id,
                );
            }
            reply(
                state
                    .daemon
                    .call(Some(project), "search_docs", json!(input), started)
                    .await,
                id,
            )
        }
        _ => failure(
            StatusCode::METHOD_NOT_ALLOWED,
            "invalid_request",
            "Method or operation not supported",
            id,
        ),
    }
}
fn reply(result: crate::error::Result<Value>, id: &str) -> Response {
    match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(error) => mapped(&error, id),
    }
}
