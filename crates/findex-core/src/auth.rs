//! Firebase-backed identity shared by the desktop, CLI, and TUI.
//!
//! OAuth runs in the system browser. A one-time loopback listener receives a
//! Google ID token from the hosted bridge, exchanges it with Firebase, and
//! stores only the refresh token in the operating-system credential vault.

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, Method, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tower_http::cors::CorsLayer;
use url::Url;

const FIREBASE_API_KEY: &str = "AIzaSyBniZaZKTefW67CxPt7YbwtaBVdc-9YiVE";
const AUTH_BRIDGE: &str = "https://findexcodeintelligence.web.app/";
const AUTH_ORIGIN: &str = "https://findexcodeintelligence.web.app";
const KEYRING_SERVICE: &str = "dev.findex.desktop";
const KEYRING_USER: &str = "firebase:refresh-token";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserProfile {
    pub uid: String,
    pub email: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub provider: String,
    pub signed_in_at: String,
}

#[derive(Debug, Clone)]
pub struct FirebaseCredential {
    pub id_token: String,
    pub refresh_token: String,
    pub expires_in_seconds: u64,
    pub user: UserProfile,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("failed to start the private sign-in callback: {0}")]
    Bind(#[source] std::io::Error),
    #[error("failed to open the system browser: {0}")]
    Browser(String),
    #[error("sign-in timed out after three minutes")]
    Timeout,
    #[error("the sign-in callback was closed before credentials arrived")]
    CallbackClosed,
    #[error("Firebase request failed: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Firebase rejected sign-in: {0}")]
    Firebase(String),
    #[error("credential vault error: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("profile storage error: {0}")]
    Storage(#[from] std::io::Error),
    #[error("profile data is invalid: {0}")]
    Profile(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
struct CallbackPayload {
    state: String,
    id_token: String,
}

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    sender: Arc<Mutex<Option<oneshot::Sender<String>>>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentityResponse {
    local_id: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    display_name: String,
    photo_url: Option<String>,
    refresh_token: String,
    id_token: String,
    expires_in: String,
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    id_token: String,
    refresh_token: String,
    expires_in: String,
}

/// Start a browser-based Google OAuth flow and persist the resulting Firebase
/// session in the OS credential vault. The callback binds only to loopback and
/// accepts a single request guarded by 122 bits of random state.
pub async fn login() -> Result<UserProfile, AuthError> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(AuthError::Bind)?;
    let port = listener.local_addr().map_err(AuthError::Bind)?.port();
    let state = uuid::Uuid::new_v4().to_string();
    let (sender, receiver) = oneshot::channel();
    let callback_state = CallbackState {
        expected_state: state.clone(),
        sender: Arc::new(Mutex::new(Some(sender))),
    };
    let cors = CorsLayer::new()
        .allow_origin(
            AUTH_ORIGIN
                .parse::<header::HeaderValue>()
                .expect("static origin"),
        )
        .allow_methods([Method::POST])
        .allow_headers([header::CONTENT_TYPE]);
    let app = Router::new()
        .route("/auth/callback", post(receive_callback))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(cors)
        .with_state(callback_state);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let callback = format!("http://127.0.0.1:{port}/auth/callback");
    let auth_url = sign_in_url(&callback, &state);
    if let Err(error) = webbrowser::open(auth_url.as_str()) {
        server.abort();
        return Err(AuthError::Browser(error.to_string()));
    }

    let google_id_token = match tokio::time::timeout(CALLBACK_TIMEOUT, receiver).await {
        Ok(Ok(token)) => token,
        Ok(Err(_)) => {
            server.abort();
            return Err(AuthError::CallbackClosed);
        }
        Err(_) => {
            server.abort();
            return Err(AuthError::Timeout);
        }
    };
    server.abort();

    let credential = exchange_google_token(&google_id_token).await?;
    persist_session(&credential)?;
    Ok(credential.user)
}

pub fn current_user() -> Result<Option<UserProfile>, AuthError> {
    let path = profile_path();
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

/// Refresh the short-lived Firebase bearer token without exposing the stored
/// refresh token to the UI or CLI output.
pub async fn credential() -> Result<Option<FirebaseCredential>, AuthError> {
    let Some(user) = current_user()? else {
        return Ok(None);
    };
    let entry = keyring_entry()?;
    let refresh_token = match entry.get_password() {
        Ok(token) => token,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let response = reqwest::Client::new()
        .post(format!(
            "https://securetoken.googleapis.com/v1/token?key={FIREBASE_API_KEY}"
        ))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
        ])
        .send()
        .await?;
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        return Err(AuthError::Firebase(bounded_error(&bytes)));
    }
    let refreshed: RefreshResponse = serde_json::from_slice(&bytes)?;
    entry.set_password(&refreshed.refresh_token)?;
    Ok(Some(FirebaseCredential {
        id_token: refreshed.id_token,
        refresh_token: refreshed.refresh_token,
        expires_in_seconds: refreshed.expires_in.parse().unwrap_or(3_600),
        user,
    }))
}

pub fn logout() -> Result<(), AuthError> {
    let entry = keyring_entry()?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(error.into()),
    }
    let path = profile_path();
    if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    Ok(())
}

pub fn profile_path() -> PathBuf {
    ProjectDirs::from("dev", "findex", "Findex")
        .map(|directories| directories.data_local_dir().join("auth-profile.json"))
        .unwrap_or_else(|| std::env::temp_dir().join("findex-auth-profile.json"))
}

fn sign_in_url(callback: &str, state: &str) -> Url {
    let mut url = Url::parse(AUTH_BRIDGE).expect("static auth bridge URL");
    url.query_pairs_mut()
        .append_pair("callback", callback)
        .append_pair("state", state);
    url
}

async fn receive_callback(
    State(state): State<CallbackState>,
    Json(payload): Json<CallbackPayload>,
) -> (StatusCode, Json<Value>) {
    if payload.state != state.expected_state || payload.id_token.len() > 8_192 {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": "invalid callback state" })),
        );
    }
    let sender = state
        .sender
        .lock()
        .expect("auth callback lock poisoned")
        .take();
    let Some(sender) = sender else {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "ok": false, "error": "callback already completed" })),
        );
    };
    if sender.send(payload.id_token).is_err() {
        return (
            StatusCode::GONE,
            Json(json!({ "ok": false, "error": "sign-in request expired" })),
        );
    }
    (StatusCode::OK, Json(json!({ "ok": true })))
}

async fn exchange_google_token(google_id_token: &str) -> Result<FirebaseCredential, AuthError> {
    let response = reqwest::Client::new()
        .post(format!(
            "https://identitytoolkit.googleapis.com/v1/accounts:signInWithIdp?key={FIREBASE_API_KEY}"
        ))
        .json(&json!({
            "postBody": format!("id_token={google_id_token}&providerId=google.com"),
            "requestUri": AUTH_BRIDGE,
            "returnIdpCredential": false,
            "returnSecureToken": true
        }))
        .send()
        .await?;
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        return Err(AuthError::Firebase(bounded_error(&bytes)));
    }
    let identity: IdentityResponse = serde_json::from_slice(&bytes)?;
    let display_name = if identity.display_name.trim().is_empty() {
        identity
            .email
            .split('@')
            .next()
            .unwrap_or("Findex user")
            .to_string()
    } else {
        identity.display_name
    };
    Ok(FirebaseCredential {
        id_token: identity.id_token,
        refresh_token: identity.refresh_token,
        expires_in_seconds: identity.expires_in.parse().unwrap_or(3_600),
        user: UserProfile {
            uid: identity.local_id,
            email: identity.email,
            display_name,
            avatar_url: identity.photo_url,
            provider: "google.com".to_string(),
            signed_in_at: now_rfc3339(),
        },
    })
}

fn persist_session(credential: &FirebaseCredential) -> Result<(), AuthError> {
    keyring_entry()?.set_password(&credential.refresh_token)?;
    let path = profile_path();
    let parent = path.parent().expect("profile path has parent");
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, &credential.user)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

fn keyring_entry() -> Result<keyring::Entry, keyring::Error> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
}

fn bounded_error(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(1_024)
        .collect::<String>()
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_url_is_bounded_to_the_hosted_bridge() {
        let url = sign_in_url(
            "http://127.0.0.1:42000/auth/callback",
            "3f20a61c-5482-47ff-82a8-8d08bdd43f16",
        );
        assert_eq!(url.origin().ascii_serialization(), AUTH_ORIGIN);
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "callback")
                .unwrap()
                .1,
            "http://127.0.0.1:42000/auth/callback"
        );
    }

    #[tokio::test]
    async fn callback_rejects_wrong_state_without_consuming_sender() {
        let (sender, receiver) = oneshot::channel();
        let state = CallbackState {
            expected_state: "expected".into(),
            sender: Arc::new(Mutex::new(Some(sender))),
        };
        let (status, _) = receive_callback(
            State(state.clone()),
            Json(CallbackPayload {
                state: "wrong".into(),
                id_token: "token".into(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(state.sender.lock().unwrap().is_some());
        drop(receiver);
    }
}
