//! Local HTTP API for apps, scripts and websites.
//!
//! Every `/v1` route requires `Authorization: Bearer <token>` (or `?token=`
//! for EventSource). The token lives in `~/.amara/api.token`. CORS is open so
//! browser apps can integrate, but nothing works without the token.
//!
//! `/ui` serves the dashboard (desktop tray and phones). It is static and holds
//! no secrets: it reads the token from its URL fragment, which browsers never
//! send to the server.

use crate::ops;
use amara_agent::Agent;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
struct AppState {
    agent: Arc<Agent>,
    token: Arc<String>,
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let msg = e.to_string();
        let code = if msg.contains("no unique peer")
            || msg.contains("no nearby agent")
            || msg.contains("not found")
        {
            StatusCode::NOT_FOUND
        } else if msg.contains("timed out") {
            StatusCode::GATEWAY_TIMEOUT
        } else {
            StatusCode::BAD_REQUEST
        };
        Self(code, msg)
    }
}

impl From<amara_core::AmaraError> for ApiError {
    fn from(e: amara_core::AmaraError) -> Self {
        anyhow::Error::from(e).into()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

pub fn router(agent: Arc<Agent>, token: String) -> Router {
    let state = AppState {
        agent,
        token: Arc::new(token),
    };
    let v1 = Router::new()
        .route("/status", get(status))
        .route("/peers", get(peers))
        .route("/send", post(send))
        .route("/call", post(call))
        .route("/card/{peer}", get(card))
        .route("/inbox", get(inbox))
        .route("/reply", post(reply))
        .route("/tools", get(tools).post(expose))
        .route("/tools/{name}", delete(unexpose))
        .route("/schema", get(schema))
        .route("/events", get(events))
        .route(
            "/files",
            get(files)
                .post(send_file)
                .layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route("/files/{name}", get(download))
        .route("/topics", get(topics))
        .route("/topics/watch", get(watch_topics))
        .route("/publish", post(publish))
        .route("/estop", post(estop))
        .layer(middleware::from_fn_with_state(state.clone(), auth));
    Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"ok": true, "version": amara_core::AMARA_VERSION})) }),
        )
        .route(
            "/ui",
            get(|| async { ui_page(UI_HTML, "text/html; charset=utf-8") }),
        )
        .route(
            "/ui/manifest.webmanifest",
            get(|| async { ui_page(UI_MANIFEST, "application/manifest+json") }),
        )
        .nest("/v1", v1)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let query = req.uri().query().and_then(|q| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("token="))
            .map(str::to_string)
    });
    let presented = bearer.or(query).unwrap_or_default();
    if constant_time_eq(presented.as_bytes(), state.token.as_bytes()) {
        next.run(req).await
    } else {
        ApiError(
            StatusCode::UNAUTHORIZED,
            "missing or invalid API token (see ~/.amara/api.token)".into(),
        )
        .into_response()
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

const UI_HTML: &str = include_str!("ui/index.html");
const UI_MANIFEST: &str = include_str!("ui/manifest.webmanifest");

fn ui_page(body: &'static str, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

async fn status(State(s): State<AppState>) -> ApiResult {
    let id = s.agent.identity();
    let held: usize = s.agent.mesh().held().iter().map(|(_, n)| n).sum();
    Ok(Json(json!({
        "name": id.display_name(),
        "id": id.peer_id().to_string(),
        "version": amara_core::AMARA_VERSION,
        "peers": s.agent.peers().len(),
        "known": s.agent.mesh().known_peers().len(),
        "held": held,
        "tools": s.agent.tools(),
        "inbox": s.agent.inbox().len(),
    })))
}

#[derive(Deserialize)]
struct PeersQuery {
    capability: Option<String>,
}

async fn peers(State(s): State<AppState>, Query(q): Query<PeersQuery>) -> ApiResult {
    Ok(Json(json!(ops::peers(&s.agent, q.capability.as_deref()))))
}

#[derive(Deserialize)]
struct SendBody {
    to: Option<String>,
    text: String,
    channel: Option<String>,
}

async fn send(State(s): State<AppState>, Json(b): Json<SendBody>) -> ApiResult {
    let status = ops::send(&s.agent, b.to.as_deref(), &b.text, b.channel.as_deref()).await?;
    Ok(Json(json!({"ok": true, "status": status})))
}

#[derive(Deserialize)]
struct CallBody {
    to: Option<String>,
    tool: String,
    #[serde(default)]
    args: Value,
    timeout_secs: Option<u64>,
    /// Answer with an SSE stream: `chunk` events, then one `result` (or `error`).
    #[serde(default)]
    stream: bool,
}

fn outcome_json(out: &ops::CallOutcome) -> Value {
    json!({"ok": out.ok, "data": out.data, "ms": out.ms, "peer": {"name": out.peer_name, "id": out.peer_id}})
}

async fn call(State(s): State<AppState>, Json(b): Json<CallBody>) -> Result<Response, ApiError> {
    if !b.stream {
        let out = ops::call(&s.agent, b.to.as_deref(), &b.tool, b.args, b.timeout_secs).await?;
        return Ok(Json(outcome_json(&out)).into_response());
    }
    // Resolve up front so a bad target is a plain HTTP error, not a stream.
    s.agent.resolve_target(b.to.as_deref(), &b.tool)?;
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        let call = ops::call_streaming(
            &s.agent,
            b.to.as_deref(),
            &b.tool,
            b.args,
            b.timeout_secs,
            Some(chunk_tx),
        );
        tokio::pin!(call);
        // Chunks and the final result come from one task, so they stay in order.
        let result = loop {
            tokio::select! {
                biased;
                Some(chunk) = chunk_rx.recv() => {
                    if let Ok(ev) = Event::default().event("chunk").json_data(&chunk) {
                        let _ = events_tx.send(ev);
                    }
                }
                result = &mut call => break result,
            }
        };
        while let Ok(chunk) = chunk_rx.try_recv() {
            if let Ok(ev) = Event::default().event("chunk").json_data(&chunk) {
                let _ = events_tx.send(ev);
            }
        }
        let ev = match result {
            Ok(out) => Event::default()
                .event("result")
                .json_data(outcome_json(&out)),
            Err(e) => Event::default()
                .event("error")
                .json_data(json!({"error": e.to_string()})),
        };
        if let Ok(ev) = ev {
            let _ = events_tx.send(ev);
        }
    });
    let stream = futures_util::stream::unfold(events_rx, |mut rx| async move {
        rx.recv().await.map(|ev| (Ok::<_, Infallible>(ev), rx))
    });
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

#[derive(Deserialize)]
struct FileBody {
    to: String,
    /// A file on this machine...
    path: Option<String>,
    /// ...or the contents, base64-encoded, with a name (for browsers and phones).
    name: Option<String>,
    data_base64: Option<String>,
}

async fn send_file(State(s): State<AppState>, Json(b): Json<FileBody>) -> ApiResult {
    let receipt = match (b.path, b.data_base64) {
        (Some(path), _) => ops::send_file(&s.agent, &b.to, &path).await?,
        (None, Some(data)) => {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.trim())
                .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("data_base64: {e}")))?;
            let peer = s.agent.resolve(&b.to)?;
            s.agent
                .send_file_bytes(peer.peer_id, b.name.as_deref().unwrap_or("file"), bytes)
                .await?
        }
        (None, None) => {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "give `path` or `data_base64`".into(),
            ));
        }
    };
    Ok(Json(json!({"ok": true, "receipt": receipt})))
}

#[derive(Deserialize)]
struct TopicQuery {
    /// `rover1/battery`, `rover1/*` or `*`.
    topic: Option<String>,
}

async fn topics(State(s): State<AppState>, Query(q): Query<TopicQuery>) -> ApiResult {
    Ok(Json(json!(s.agent.topics(q.topic.as_deref()))))
}

/// SSE: every sample matching `?topic=` as it arrives (`sample` events).
async fn watch_topics(
    State(s): State<AppState>,
    Query(q): Query<TopicQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let pattern = q.topic.unwrap_or_else(|| "*".into());
    let rx = s.agent.subscribe_topics();
    let stream = futures_util::stream::unfold((rx, pattern), |(mut rx, pattern)| async move {
        loop {
            match rx.recv().await {
                Ok(sample) if amara_runtime::topic_matches(&pattern, &sample.topic) => {
                    if let Ok(ev) = Event::default().event("sample").json_data(&sample) {
                        return Some((Ok(ev), (rx, pattern)));
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct PublishBody {
    topic: String,
    #[serde(default)]
    data: Value,
    hops: Option<u8>,
}

async fn publish(State(s): State<AppState>, Json(b): Json<PublishBody>) -> ApiResult {
    let seq = s.agent.publish(&b.topic, b.data, b.hops).await?;
    Ok(Json(json!({"ok": true, "seq": seq})))
}

#[derive(Deserialize)]
struct EstopBody {
    to: Option<String>,
    #[serde(default)]
    reason: String,
}

async fn estop(State(s): State<AppState>, Json(b): Json<EstopBody>) -> ApiResult {
    let reason = if b.reason.is_empty() {
        "emergency stop"
    } else {
        &b.reason
    };
    let status = ops::estop(&s.agent, b.to.as_deref(), reason).await?;
    Ok(Json(json!({"ok": true, "status": status})))
}

fn received_dir(s: &AppState) -> Option<std::path::PathBuf> {
    s.agent.config().files.dir.clone()
}

/// Files received so far, newest first.
async fn files(State(s): State<AppState>) -> ApiResult {
    let mut list = Vec::new();
    if let Some(dir) = received_dir(&s)
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(meta) = entry.metadata() else { continue };
            if name.starts_with('.') || !meta.is_file() {
                continue; // transfers in progress
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            list.push(json!({"name": name, "size": meta.len(), "modified": modified}));
        }
    }
    list.sort_by_key(|f| std::cmp::Reverse(f["modified"].as_u64()));
    Ok(Json(json!(list)))
}

async fn download(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let not_found = || ApiError(StatusCode::NOT_FOUND, format!("no received file '{name}'"));
    // Only plain names inside the download directory.
    if amara_runtime::sanitize_file_name(&name) != name {
        return Err(not_found());
    }
    let path = received_dir(&s).ok_or_else(not_found)?.join(&name);
    let bytes = tokio::fs::read(&path).await.map_err(|_| not_found())?;
    let disposition = format!("attachment; filename=\"{}\"", name.replace('"', ""));
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    )
        .into_response())
}

async fn card(State(s): State<AppState>, Path(peer): Path<String>) -> ApiResult {
    Ok(Json(ops::card(&s.agent, &peer).await?))
}

#[derive(Deserialize)]
struct InboxQuery {
    wait: Option<u64>,
    max: Option<usize>,
}

async fn inbox(State(s): State<AppState>, Query(q): Query<InboxQuery>) -> ApiResult {
    Ok(Json(json!(
        ops::inbox(&s.agent, q.wait.unwrap_or(0), q.max.unwrap_or(50)).await
    )))
}

async fn reply(State(s): State<AppState>, Json(b): Json<Value>) -> ApiResult {
    Ok(Json(
        json!({"ok": true, "status": ops::reply(&s.agent, &b)?}),
    ))
}

async fn tools(State(s): State<AppState>) -> ApiResult {
    Ok(Json(json!(s.agent.tools())))
}

async fn expose(State(s): State<AppState>, Json(b): Json<Value>) -> ApiResult {
    Ok(Json(
        json!({"ok": true, "status": ops::expose(&s.agent, &b)?}),
    ))
}

async fn unexpose(State(s): State<AppState>, Path(name): Path<String>) -> ApiResult {
    Ok(Json(json!({"ok": s.agent.remove_tool(&name)})))
}

#[derive(Deserialize)]
struct SchemaQuery {
    format: Option<String>,
}

async fn schema(Query(q): Query<SchemaQuery>) -> ApiResult {
    Ok(Json(ops::schema(q.format.as_deref().unwrap_or("openai"))?))
}

/// Server-sent events: `inbox` (messages/calls as they arrive) and `mesh` (peer/security events).
/// Observing does not consume the inbox.
async fn events(State(s): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let inbox_rx = s.agent.inbox().subscribe();
    let mesh_rx = s.agent.events();
    let stream =
        futures_util::stream::unfold((inbox_rx, mesh_rx), |(mut inbox, mut mesh)| async move {
            loop {
                let event = tokio::select! {
                    item = inbox.recv() => match item {
                        Ok(item) => Event::default().event("inbox").json_data(&item).ok(),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => return None,
                    },
                    ev = mesh.recv() => match ev {
                        Ok(ev) => Event::default().event("mesh").json_data(&ev).ok(),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => return None,
                    },
                };
                if let Some(event) = event {
                    return Some((Ok(event), (inbox, mesh)));
                }
            }
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
