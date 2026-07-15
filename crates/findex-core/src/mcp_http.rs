//! MCP Streamable HTTP transport for local and remote agent clients.

use crate::mcp::McpServer;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

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
}

pub async fn serve(server: McpServer, config: McpHttpConfig) -> anyhow::Result<()> {
    if !config.bind.ip().is_loopback() && config.bearer_token.is_none() {
        anyhow::bail!("a bearer token is required when MCP HTTP binds beyond loopback");
    }
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let state = HttpState {
        server,
        config: Arc::new(config),
    };
    let app = Router::new()
        .route("/mcp", post(handle_mcp))
        .route("/health", get(health))
        .with_state(state);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({
        "service": "findex-mcp",
        "status": "ok",
        "protocolVersion": MCP_PROTOCOL_VERSION
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
    match state.server.handle_json(payload) {
        Some(response) => {
            let mut response = (StatusCode::OK, Json(response)).into_response();
            response.headers_mut().insert(
                "mcp-protocol-version",
                MCP_PROTOCOL_VERSION.parse().expect("static header"),
            );
            response
        }
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn validate_headers(headers: &HeaderMap, config: &McpHttpConfig) -> Result<(), Box<Response>> {
    if let Some(version) = headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok())
    {
        if version != MCP_PROTOCOL_VERSION {
            return Err(Box::new(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "unsupported MCP protocol version" })),
                )
                    .into_response(),
            ));
        }
    }
    if let Some(expected) = config.bearer_token.as_deref() {
        let valid = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()));
        if !valid {
            return Err(Box::new(StatusCode::UNAUTHORIZED.into_response()));
        }
    }
    if let Some(origin) = headers.get("origin").and_then(|value| value.to_str().ok()) {
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
}
