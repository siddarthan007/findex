//! MCP Streamable HTTP transport with bounded sessions and replayable SSE events.

use crate::mcp::McpServer;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const SESSION_HEADER: &str = "mcp-session-id";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_SESSIONS: usize = 64;
const MAX_REPLAY_EVENTS: usize = 256;
const MAX_REPLAY_BYTES: usize = 1024 * 1024;
const MAX_EVENT_BYTES: usize = 256 * 1024;
const SESSION_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpHttpConfig {
    pub bind: SocketAddr,
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

impl Default for McpHttpConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:37420".parse().expect("valid default address"),
            bearer_token: None,
            allowed_origins: vec![
                "tauri://localhost".to_string(),
                "http://tauri.localhost".to_string(),
            ],
        }
    }
}

#[derive(Clone)]
struct HttpState {
    server: McpServer,
    config: Arc<McpHttpConfig>,
    sessions: Arc<Mutex<SessionStore>>,
}

struct StoredEvent {
    sequence: u64,
    id: String,
    data: String,
}

struct Session {
    last_seen: Instant,
    next_sequence: u64,
    events: VecDeque<StoredEvent>,
    replay_bytes: usize,
}

#[derive(Default)]
struct SessionStore {
    sessions: HashMap<String, Session>,
}

impl SessionStore {
    fn cleanup(&mut self) {
        self.sessions
            .retain(|_, session| session.last_seen.elapsed() < SESSION_TTL);
        if self.sessions.len() > MAX_SESSIONS {
            let mut by_age = self
                .sessions
                .iter()
                .map(|(id, session)| (id.clone(), session.last_seen))
                .collect::<Vec<_>>();
            by_age.sort_by_key(|(_, seen)| *seen);
            for (id, _) in by_age.into_iter().take(self.sessions.len() - MAX_SESSIONS) {
                self.sessions.remove(&id);
            }
        }
    }

    fn create(&mut self) -> String {
        self.cleanup();
        let id = uuid::Uuid::new_v4().to_string();
        self.sessions.insert(
            id.clone(),
            Session {
                last_seen: Instant::now(),
                next_sequence: 1,
                events: VecDeque::new(),
                replay_bytes: 0,
            },
        );
        self.cleanup();
        id
    }

    fn touch(&mut self, id: &str) -> bool {
        self.cleanup();
        let Some(session) = self.sessions.get_mut(id) else {
            return false;
        };
        session.last_seen = Instant::now();
        true
    }

    fn store_event(&mut self, session_id: &str, data: String) -> Option<String> {
        if data.len() > MAX_EVENT_BYTES {
            return None;
        }
        let session = self.sessions.get_mut(session_id)?;
        session.last_seen = Instant::now();
        let sequence = session.next_sequence;
        session.next_sequence = sequence.saturating_add(1);
        let id = format!("{session_id}:{sequence}");
        session.replay_bytes = session.replay_bytes.saturating_add(data.len());
        session.events.push_back(StoredEvent {
            sequence,
            id: id.clone(),
            data,
        });
        while session.events.len() > MAX_REPLAY_EVENTS || session.replay_bytes > MAX_REPLAY_BYTES {
            if let Some(removed) = session.events.pop_front() {
                session.replay_bytes = session.replay_bytes.saturating_sub(removed.data.len());
            }
        }
        Some(id)
    }

    fn replay(&mut self, session_id: &str, after: Option<u64>) -> Result<String, ReplayError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or(ReplayError::MissingSession)?;
        session.last_seen = Instant::now();
        let oldest = session
            .events
            .front()
            .map(|event| event.sequence)
            .unwrap_or(session.next_sequence);
        if let Some(after) = after {
            if after >= session.next_sequence || after.saturating_add(1) < oldest {
                return Err(ReplayError::UnavailableHistory);
            }
        }
        let after = after.unwrap_or(0);
        let mut body = String::new();
        for event in session.events.iter().filter(|event| event.sequence > after) {
            append_sse(&mut body, Some(&event.id), &event.data);
        }
        Ok(body)
    }
}

#[derive(Debug)]
enum ReplayError {
    MissingSession,
    UnavailableHistory,
}

pub async fn serve(server: McpServer, config: McpHttpConfig) -> anyhow::Result<()> {
    if !config.bind.ip().is_loopback() && config.bearer_token.is_none() {
        anyhow::bail!("a bearer token is required when MCP HTTP binds beyond loopback");
    }
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let state = HttpState {
        server,
        config: Arc::new(config),
        sessions: Arc::new(Mutex::new(SessionStore::default())),
    };
    let app = Router::new()
        .route(
            "/mcp",
            post(handle_mcp).get(handle_stream).delete(handle_delete),
        )
        .route("/health", get(health))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .with_state(state);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({
        "service": "findex-mcp",
        "status": "ok",
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "transport": "streamable-http"
    }))
}

async fn handle_mcp(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    if let Err(response) = validate_headers(&headers, &state.config) {
        return *response;
    }
    let initialize = payload.get("method").and_then(Value::as_str) == Some("initialize");
    let session_id = if initialize {
        None
    } else {
        match require_session(&headers, &state.sessions) {
            Ok(id) => Some(id),
            Err(response) => return *response,
        }
    };
    if !initialize && !protocol_header_matches(&headers) {
        return protocol_error();
    }

    let server = state.server.clone();
    let result = match tokio::task::spawn_blocking(move || server.handle_json(payload)).await {
        Ok(result) => result,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("MCP worker failed: {error}") })),
            )
                .into_response()
        }
    };
    let session_id = session_id.or_else(|| {
        result.as_ref().and_then(|response| {
            response
                .get("result")
                .and_then(|_| state.sessions.lock().ok().map(|mut store| store.create()))
        })
    });
    let wants_sse = accepts(&headers, "text/event-stream");
    match result {
        Some(value) if wants_sse => {
            let data = serde_json::to_string(&value).expect("JSON value serializes");
            let event_id = session_id.as_ref().and_then(|id| {
                state
                    .sessions
                    .lock()
                    .ok()
                    .and_then(|mut store| store.store_event(id, data.clone()))
            });
            let mut body = String::new();
            append_sse(&mut body, event_id.as_deref(), &data);
            stream_response(StatusCode::OK, body, session_id.as_deref())
        }
        Some(value) => {
            let mut response = (StatusCode::OK, Json(value)).into_response();
            add_protocol_headers(&mut response, session_id.as_deref());
            response
        }
        None => {
            let mut response = StatusCode::ACCEPTED.into_response();
            add_protocol_headers(&mut response, session_id.as_deref());
            response
        }
    }
}

async fn handle_stream(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_headers(&headers, &state.config) {
        return *response;
    }
    if !protocol_header_matches(&headers) {
        return protocol_error();
    }
    if !accepts(&headers, "text/event-stream") {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    }
    let session_id = match require_session(&headers, &state.sessions) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let after = match last_event_sequence(&headers, &session_id) {
        Ok(sequence) => sequence,
        Err(response) => return *response,
    };
    let mut body = match state
        .sessions
        .lock()
        .map_err(|_| ReplayError::MissingSession)
        .and_then(|mut store| store.replay(&session_id, after))
    {
        Ok(body) => body,
        Err(ReplayError::MissingSession) => return StatusCode::NOT_FOUND.into_response(),
        Err(ReplayError::UnavailableHistory) => return StatusCode::CONFLICT.into_response(),
    };
    if body.is_empty() {
        body.push_str(": replay-ready\nretry: 1000\n\n");
    }
    stream_response(StatusCode::OK, body, Some(&session_id))
}

async fn handle_delete(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_headers(&headers, &state.config) {
        return *response;
    }
    if !protocol_header_matches(&headers) {
        return protocol_error();
    }
    let Some(session_id) = header_text(&headers, SESSION_HEADER) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let removed = state
        .sessions
        .lock()
        .map(|mut store| store.sessions.remove(session_id).is_some())
        .unwrap_or(false);
    if removed {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

fn require_session(
    headers: &HeaderMap,
    sessions: &Mutex<SessionStore>,
) -> Result<String, Box<Response>> {
    let id = header_text(headers, SESSION_HEADER)
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
        .ok_or_else(|| Box::new(StatusCode::BAD_REQUEST.into_response()))?;
    let active = sessions
        .lock()
        .map(|mut store| store.touch(id))
        .unwrap_or(false);
    if !active {
        return Err(Box::new(StatusCode::NOT_FOUND.into_response()));
    }
    Ok(id.to_string())
}

fn last_event_sequence(
    headers: &HeaderMap,
    session_id: &str,
) -> Result<Option<u64>, Box<Response>> {
    let Some(id) = header_text(headers, "last-event-id") else {
        return Ok(None);
    };
    let Some((event_session, sequence)) = id.rsplit_once(':') else {
        return Err(Box::new(StatusCode::BAD_REQUEST.into_response()));
    };
    if event_session != session_id {
        return Err(Box::new(StatusCode::BAD_REQUEST.into_response()));
    }
    sequence
        .parse::<u64>()
        .map(Some)
        .map_err(|_| Box::new(StatusCode::BAD_REQUEST.into_response()))
}

fn append_sse(body: &mut String, id: Option<&str>, data: &str) {
    if let Some(id) = id {
        body.push_str("id: ");
        body.push_str(id);
        body.push('\n');
    }
    body.push_str("retry: 1000\ndata: ");
    body.push_str(data);
    body.push_str("\n\n");
}

fn stream_response(status: StatusCode, body: String, session_id: Option<&str>) -> Response {
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header("x-accel-buffering", "no")
        .body(Body::from(body))
        .expect("static response headers");
    add_protocol_headers(&mut response, session_id);
    response
}

fn add_protocol_headers(response: &mut Response, session_id: Option<&str>) {
    response.headers_mut().insert(
        HeaderName::from_static("mcp-protocol-version"),
        HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );
    if let Some(session_id) = session_id.and_then(|id| HeaderValue::from_str(id).ok()) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(SESSION_HEADER), session_id);
    }
}

fn protocol_error() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "missing or unsupported MCP protocol version" })),
    )
        .into_response()
}

fn protocol_header_matches(headers: &HeaderMap) -> bool {
    header_text(headers, "mcp-protocol-version") == Some(MCP_PROTOCOL_VERSION)
}

fn accepts(headers: &HeaderMap, media_type: &str) -> bool {
    header_text(headers, "accept").is_some_and(|value| {
        value
            .split(',')
            .any(|item| item.trim().split(';').next() == Some(media_type))
    })
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn validate_headers(headers: &HeaderMap, config: &McpHttpConfig) -> Result<(), Box<Response>> {
    if let Some(version) = header_text(headers, "mcp-protocol-version") {
        if version != MCP_PROTOCOL_VERSION {
            return Err(Box::new(protocol_error()));
        }
    }
    if let Some(expected) = config.bearer_token.as_deref() {
        let valid = header_text(headers, "authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()));
        if !valid {
            return Err(Box::new(StatusCode::UNAUTHORIZED.into_response()));
        }
    }
    if let Some(origin) = header_text(headers, "origin") {
        let loopback_dev = origin
            .strip_prefix("http://localhost:")
            .or_else(|| origin.strip_prefix("http://127.0.0.1:"))
            .is_some_and(|port| port.parse::<u16>().is_ok());
        let allowed = loopback_dev
            || config
                .allowed_origins
                .iter()
                .any(|candidate| candidate == origin);
        if !allowed {
            return Err(Box::new(StatusCode::FORBIDDEN.into_response()));
        }
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_and_origin_policy_are_strict() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"Secret"));
        assert!(!constant_time_eq(b"short", b"longer"));

        let mut headers = HeaderMap::new();
        headers.insert("origin", "https://evil.example".parse().unwrap());
        assert!(validate_headers(&headers, &McpHttpConfig::default()).is_err());
        headers.insert("origin", "http://127.0.0.1:1420".parse().unwrap());
        assert!(validate_headers(&headers, &McpHttpConfig::default()).is_ok());
    }

    #[test]
    fn replay_is_bounded_and_session_specific() {
        let mut store = SessionStore::default();
        let session = store.create();
        let first = store
            .store_event(&session, "{\"one\":1}".to_string())
            .unwrap();
        store.store_event(&session, "{\"two\":2}".to_string());
        assert!(store.replay(&session, Some(1)).unwrap().contains("two"));
        assert_eq!(
            last_event_sequence(
                &{
                    let mut headers = HeaderMap::new();
                    headers.insert("last-event-id", first.parse().unwrap());
                    headers
                },
                &session
            )
            .unwrap(),
            Some(1)
        );
    }

    #[test]
    fn replay_rejects_evicted_and_future_event_ids() {
        let mut store = SessionStore::default();
        let session = store.create();
        for sequence in 0..=MAX_REPLAY_EVENTS {
            store
                .store_event(&session, format!("{{\"sequence\":{sequence}}}"))
                .unwrap();
        }
        assert!(matches!(
            store.replay(&session, Some(0)),
            Err(ReplayError::UnavailableHistory)
        ));
        assert!(matches!(
            store.replay(&session, Some(u64::MAX)),
            Err(ReplayError::UnavailableHistory)
        ));
    }
}
