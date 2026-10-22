//! Local HTTP API for apps, scripts and websites.
//!
//! Every `/v1` route requires `Authorization: Bearer <token>` (or `?token=`
//! for EventSource). The token lives in `~/.amara/api.token`. CORS is open so
//! browser apps can integrate, but nothing works without the token.

use crate::ops;
use amara_agent::Agent;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
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
        .layer(middleware::from_fn_with_state(state.clone(), auth));
    Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"ok": true, "version": amara_core::AMARA_VERSION})) }),
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

async fn status(State(s): State<AppState>) -> ApiResult {
    let id = s.agent.identity();
    Ok(Json(json!({
        "name": id.display_name(),
        "id": id.peer_id().to_string(),
        "version": amara_core::AMARA_VERSION,
        "peers": s.agent.peers().len(),
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
}

async fn call(State(s): State<AppState>, Json(b): Json<CallBody>) -> ApiResult {
    let out = ops::call(&s.agent, b.to.as_deref(), &b.tool, b.args, b.timeout_secs).await?;
    Ok(Json(
        json!({"ok": out.ok, "data": out.data, "ms": out.ms, "peer": {"name": out.peer_name, "id": out.peer_id}}),
    ))
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
