#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use axum::extract::State as AxumState;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use findex_core::graph_query::query_graph;
use findex_core::intelligence::{
    architecture_overview, ast_outline, graph_snapshot, impact_analysis, ArchitectureOverview,
    AstOutline, GraphSnapshot, ImpactReport,
};
use findex_core::search::local_embedder::create_embedder;
use findex_core::search::rerank::{create_reranker, Reranker};
use findex_core::search::vector::Embedder;
use findex_core::storage::{Storage, Symbol};
use findex_core::{ingest_codebase, search_codebase_with_components};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};
use tower_http::cors::CorsLayer;

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
    storage: Arc<Storage>,
    reranker: Arc<dyn Reranker>,
    embedder: Arc<dyn Embedder>,
    api_url: String,
    api_token: String,
}

#[derive(Default)]
struct PendingDesktopUpdate(Mutex<Option<Update>>);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopUpdateInfo {
    version: String,
    current_version: String,
    notes: String,
    date: Option<String>,
    target: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiConfig {
    base_url: String,
    token: String,
}

#[derive(Debug, Serialize)]
struct StatsView {
    files: usize,
    symbols: usize,
    edges: usize,
    vectors: usize,
    merkle_root: Option<String>,
    stack_graphs: Option<findex_core::stack_graphs::StackGraphStats>,
}

#[derive(Debug, Deserialize)]
struct SearchRequest {
    query: String,
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_mode() -> String {
    "hybrid".to_string()
}

fn default_limit() -> usize {
    25
}

#[derive(Debug, Serialize)]
struct SearchResult {
    score: f32,
    symbol: Symbol,
}

#[derive(Debug, Deserialize)]
struct QueryRequest {
    query: String,
}

#[derive(Debug, Deserialize)]
struct PathRequest {
    path: String,
}

#[derive(Debug, Deserialize)]
struct SymbolRequest {
    symbol_id: String,
}

fn stats(state: &AppState) -> Result<StatsView, String> {
    let files = state
        .storage
        .list_files()
        .map_err(|error| error.to_string())?;
    let symbols = state
        .storage
        .list_symbols()
        .map_err(|error| error.to_string())?;
    let edges = state
        .storage
        .list_edges()
        .map_err(|error| error.to_string())?;
    let vectors = if state.db_path.join("vector").exists() {
        symbols.len()
    } else {
        0
    };
    Ok(StatsView {
        files: files.len(),
        symbols: symbols.len(),
        edges: edges.len(),
        vectors,
        merkle_root: state
            .storage
            .get_metadata::<findex_core::merkle::MerkleSnapshot>("merkle:v1")
            .map_err(|error| error.to_string())?
            .map(|snapshot| snapshot.root_hash_hex()),
        stack_graphs: state
            .storage
            .get_metadata("stack-graphs:last")
            .map_err(|error| error.to_string())?,
    })
}

fn search(state: &AppState, request: SearchRequest) -> Result<Vec<SearchResult>, String> {
    search_codebase_with_components(
        &state.db_path,
        &state.storage,
        &request.query,
        &request.mode,
        Some(state.reranker.as_ref()),
        state.embedder.as_ref(),
        request.limit.clamp(1, 100),
    )
    .map(|results| {
        results
            .into_iter()
            .map(|(symbol, score)| SearchResult { score, symbol })
            .collect()
    })
    .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_api_config(state: State<'_, AppState>) -> ApiConfig {
    ApiConfig {
        base_url: state.api_url.clone(),
        token: state.api_token.clone(),
    }
}

#[tauri::command]
fn get_graph_data(state: State<'_, AppState>, limit: usize) -> Result<GraphSnapshot, String> {
    graph_snapshot(&state.storage, limit.clamp(1, 10_000)).map_err(|error| error.to_string())
}

#[tauri::command]
fn get_architecture(state: State<'_, AppState>) -> Result<ArchitectureOverview, String> {
    architecture_overview(&state.storage).map_err(|error| error.to_string())
}

#[tauri::command]
fn search_symbols(
    state: State<'_, AppState>,
    query: String,
    mode: String,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    search(&state, SearchRequest { query, mode, limit })
}

#[tauri::command]
fn get_stats(state: State<'_, AppState>) -> Result<StatsView, String> {
    stats(&state)
}

#[tauri::command]
fn get_ast(state: State<'_, AppState>, path: String) -> Result<AstOutline, String> {
    ast_outline(&state.storage, Path::new(&path)).map_err(|error| error.to_string())
}

#[tauri::command]
fn run_graph_query(state: State<'_, AppState>, query: String) -> Result<String, String> {
    query_graph(&state.storage, &query)
        .map(|result| result.to_text())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn inspect_impact(state: State<'_, AppState>, symbol_id: String) -> Result<ImpactReport, String> {
    impact_analysis(&state.storage, &symbol_id).map_err(|error| error.to_string())
}

#[tauri::command]
fn reindex(state: State<'_, AppState>, root: String) -> Result<Value, String> {
    ingest_codebase(root, &state.db_path, &state.storage)
        .map(|stats| json!(stats))
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn check_for_update(
    app: AppHandle,
    pending: State<'_, PendingDesktopUpdate>,
) -> Result<Option<DesktopUpdateInfo>, String> {
    if !findex_core::updater::updater_enabled() {
        return Ok(None);
    }
    let endpoint = tauri::Url::parse(&format!(
        "https://github.com/{}/releases/latest/download/latest.json",
        findex_core::updater::updater_repository()
    ))
    .map_err(|error| error.to_string())?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;
    let update = updater.check().await.map_err(|error| error.to_string())?;
    let Some(update) = update else {
        *pending.0.lock().map_err(|error| error.to_string())? = None;
        return Ok(None);
    };
    let info = DesktopUpdateInfo {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update.body.clone().unwrap_or_default(),
        date: update.date.map(|date| date.to_string()),
        target: update.target.clone(),
    };
    *pending.0.lock().map_err(|error| error.to_string())? = Some(update);
    Ok(Some(info))
}

#[tauri::command]
async fn install_update(pending: State<'_, PendingDesktopUpdate>) -> Result<(), String> {
    let update = pending
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .take()
        .ok_or_else(|| "no checked update is pending; check again first".to_string())?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|error| error.to_string())
}

async fn api_stats(AxumState(state): AxumState<Arc<AppState>>, headers: HeaderMap) -> Response {
    authorized_json(&state, &headers, || stats(&state))
}

async fn api_graph(AxumState(state): AxumState<Arc<AppState>>, headers: HeaderMap) -> Response {
    authorized_json(&state, &headers, || {
        graph_snapshot(&state.storage, 2_000).map_err(|error| error.to_string())
    })
}

async fn api_runtime(AxumState(state): AxumState<Arc<AppState>>, headers: HeaderMap) -> Response {
    authorized_json(&state, &headers, || Ok(findex_core::runtime::profile(true)))
}

async fn api_architecture(
    AxumState(state): AxumState<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    authorized_json(&state, &headers, || {
        architecture_overview(&state.storage).map_err(|error| error.to_string())
    })
}

async fn api_search(
    AxumState(state): AxumState<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SearchRequest>,
) -> Response {
    authorized_json(&state, &headers, || search(&state, request))
}

async fn api_query(
    AxumState(state): AxumState<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<QueryRequest>,
) -> Response {
    authorized_json(&state, &headers, || {
        query_graph(&state.storage, &request.query)
            .map(|result| json!({ "text": result.to_text() }))
            .map_err(|error| error.to_string())
    })
}

async fn api_ast(
    AxumState(state): AxumState<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<PathRequest>,
) -> Response {
    authorized_json(&state, &headers, || {
        ast_outline(&state.storage, Path::new(&request.path)).map_err(|error| error.to_string())
    })
}

async fn api_impact(
    AxumState(state): AxumState<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SymbolRequest>,
) -> Response {
    authorized_json(&state, &headers, || {
        impact_analysis(&state.storage, &request.symbol_id).map_err(|error| error.to_string())
    })
}

fn authorized_json<T: Serialize>(
    state: &AppState,
    headers: &HeaderMap,
    action: impl FnOnce() -> Result<T, String>,
) -> Response {
    let token = headers
        .get("x-findex-token")
        .and_then(|value| value.to_str().ok());
    if token != Some(state.api_token.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match action() {
        Ok(value) => Json(value).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response(),
    }
}

async fn serve_api(state: Arc<AppState>, bind: String) -> Result<(), String> {
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            HeaderName::from_static("x-findex-token"),
        ])
        .allow_origin([
            HeaderValue::from_static("http://localhost:1420"),
            HeaderValue::from_static("tauri://localhost"),
            HeaderValue::from_static("http://tauri.localhost"),
        ]);
    let app = Router::new()
        .route("/api/stats", get(api_stats))
        .route("/api/graph", get(api_graph))
        .route("/api/runtime", get(api_runtime))
        .route("/api/architecture", get(api_architecture))
        .route("/api/search", post(api_search))
        .route("/api/query", post(api_query))
        .route("/api/ast", post(api_ast))
        .route("/api/impact", post(api_impact))
        .layer(cors)
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|error| error.to_string())?;
    axum::serve(listener, app)
        .await
        .map_err(|error| error.to_string())
}

fn main() {
    findex_core::runtime::configure_runtime();
    let db_path = std::env::var("FINDEX_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".findex_db"));
    let port = std::env::var("FINDEX_DASHBOARD_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(37_421);
    let bind = format!("127.0.0.1:{port}");
    let reranker = create_reranker();
    let embedder = create_embedder(128);
    findex_core::runtime::start_model_idle_janitor(&embedder, &reranker);
    let state = AppState {
        db_path: db_path.clone(),
        storage: Arc::new(Storage::open(&db_path).expect("failed to open findex database")),
        reranker,
        embedder,
        api_url: format!("http://{bind}"),
        api_token: uuid::Uuid::new_v4().to_string(),
    };
    let server_state = Arc::new(state.clone());
    let updater_plugin = tauri_plugin_updater::Builder::new()
        .pubkey(findex_core::updater::updater_public_key().unwrap_or_default())
        .build();
    tauri::Builder::default()
        .plugin(updater_plugin)
        .manage(state)
        .manage(PendingDesktopUpdate::default())
        .setup(move |_app| {
            tauri::async_runtime::spawn(async move {
                if let Err(error) = serve_api(server_state, bind).await {
                    eprintln!("Findex dashboard API stopped: {error}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_api_config,
            get_graph_data,
            get_architecture,
            search_symbols,
            get_stats,
            get_ast,
            run_graph_query,
            inspect_impact,
            reindex,
            check_for_update,
            install_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
