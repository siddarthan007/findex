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
use findex_core::settings::FindexSettings;
use findex_core::storage::{Storage, Symbol};
use findex_core::{
    ingest_codebase_with_options, search_codebase_with_options, IngestionOptions, SearchOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_updater::{Update, UpdaterExt};
use tower_http::cors::CorsLayer;

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
    storage: Arc<Storage>,
    reranker: Arc<dyn Reranker>,
    embedder: Arc<dyn Embedder>,
    settings: Arc<RwLock<FindexSettings>>,
    api_url: String,
    api_token: String,
    quitting: Arc<AtomicBool>,
}

#[derive(Default)]
struct PendingDesktopUpdate(Mutex<Option<Update>>);

#[derive(Default)]
struct PendingDeepLink(Mutex<Option<DeepLinkPayload>>);

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeepLinkPayload {
    url: String,
    route: String,
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

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn dispatch_deep_link(app: &AppHandle, url: &tauri::Url) -> Result<(), String> {
    if url.scheme() != "findex" || url.as_str().len() > 8_192 {
        return Err("unsupported or oversized Findex deep link".to_string());
    }
    let route = url
        .host_str()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        route.as_str(),
        "search" | "open" | "symbol" | "graph" | "settings" | "auth"
    ) {
        return Err("unsupported Findex deep-link route".to_string());
    }
    let payload = DeepLinkPayload {
        url: url.to_string(),
        route,
    };
    if let Some(pending) = app.try_state::<PendingDeepLink>() {
        *pending.0.lock().map_err(|error| error.to_string())? = Some(payload.clone());
    }
    show_main_window(app);
    app.emit("findex-deep-link", payload)
        .map_err(|error| error.to_string())
}

fn setup_deep_links(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // Installed bundles register the static scheme. Runtime registration also supports
    // portable Windows/Linux builds and development without weakening URL validation.
    #[cfg(any(windows, target_os = "linux"))]
    app.deep_link().register_all()?;

    let app_handle = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            if let Err(error) = dispatch_deep_link(&app_handle, &url) {
                eprintln!("ignored invalid Findex deep link: {error}");
            }
        }
    });

    if let Some(urls) = app.deep_link().get_current()? {
        for url in urls {
            if let Err(error) = dispatch_deep_link(app.handle(), &url) {
                eprintln!("ignored invalid startup deep link: {error}");
            }
        }
    }
    Ok(())
}

fn setup_tray(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "show", "Show Findex", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &settings, &quit])?;
    let mut tray = TrayIconBuilder::new()
        .tooltip("Findex code intelligence")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            "settings" => {
                if let Ok(url) = tauri::Url::parse("findex://settings") {
                    let _ = dispatch_deep_link(app, &url);
                }
            }
            "quit" => {
                if let Some(state) = app.try_state::<AppState>() {
                    state.quitting.store(true, Ordering::Release);
                }
                app.exit(0);
            }
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
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

#[derive(Debug, Deserialize)]
struct SourceRequest {
    path: String,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Serialize)]
struct SourcePreview {
    path: String,
    start_line: usize,
    end_line: usize,
    text: String,
    truncated: bool,
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
    let settings = state
        .settings
        .read()
        .map_err(|error| error.to_string())?
        .clone();
    search_codebase_with_options(
        &state.db_path,
        &state.storage,
        &request.query,
        &request.mode,
        Some(state.reranker.as_ref()),
        state.embedder.as_ref(),
        request.limit.clamp(1, 100),
        SearchOptions::from(&settings),
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
fn take_pending_deep_link(
    pending: State<'_, PendingDeepLink>,
) -> Result<Option<DeepLinkPayload>, String> {
    pending
        .0
        .lock()
        .map_err(|error| error.to_string())
        .map(|mut value| value.take())
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
    let settings = state
        .settings
        .read()
        .map_err(|error| error.to_string())?
        .clone();
    ingest_codebase_with_options(
        root,
        &state.db_path,
        &state.storage,
        IngestionOptions {
            build_lexical_index: settings.indexing.lexical_index,
            build_vector_index: false,
            resolve_stack_graphs: settings.indexing.stack_graphs,
        },
    )
    .map(|stats| json!(stats))
    .map_err(|error| error.to_string())
}

fn read_settings(state: &AppState) -> Result<FindexSettings, String> {
    state
        .settings
        .read()
        .map(|settings| settings.clone())
        .map_err(|error| error.to_string())
}

fn persist_settings(state: &AppState, settings: FindexSettings) -> Result<FindexSettings, String> {
    let previous = read_settings(state)?;
    let settings =
        findex_core::settings::save(&state.db_path, settings).map_err(|error| error.to_string())?;
    findex_core::runtime::apply_runtime_settings(&settings);
    if previous.runtime.compute_device != settings.runtime.compute_device {
        state
            .embedder
            .release_idle_resources(std::time::Duration::ZERO);
        state
            .reranker
            .release_idle_resources(std::time::Duration::ZERO);
    }
    *state.settings.write().map_err(|error| error.to_string())? = settings.clone();
    Ok(settings)
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Result<FindexSettings, String> {
    read_settings(&state)
}

#[tauri::command]
fn list_models() -> Vec<findex_core::models::ModelStatus> {
    findex_core::models::model_catalog_status()
}

#[tauri::command]
async fn download_model_profile(
    profile: String,
) -> Result<Vec<findex_core::models::ResolvedModel>, String> {
    let profile = profile
        .parse::<findex_core::models::ModelProfile>()
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        [
            findex_core::models::ModelKind::Embedding,
            findex_core::models::ModelKind::Reranker,
        ]
        .into_iter()
        .map(|kind| {
            findex_core::models::ensure_model_for_profile(kind, profile, false)
                .map_err(|error| error.to_string())
        })
        .collect()
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn set_settings(
    state: State<'_, AppState>,
    settings: FindexSettings,
) -> Result<FindexSettings, String> {
    persist_settings(&state, settings)
}

#[tauri::command]
async fn auth_login() -> Result<findex_core::auth::UserProfile, String> {
    findex_core::auth::login()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn auth_status() -> Result<Option<findex_core::auth::UserProfile>, String> {
    findex_core::auth::current_user().map_err(|error| error.to_string())
}

#[tauri::command]
fn auth_logout() -> Result<(), String> {
    findex_core::auth::logout().map_err(|error| error.to_string())
}

#[tauri::command]
fn telemetry_status(
    state: State<'_, AppState>,
) -> Result<findex_core::telemetry::TelemetryStatus, String> {
    let settings = read_settings(&state)?;
    findex_core::telemetry::status(&state.db_path, &settings).map_err(|error| error.to_string())
}

#[tauri::command]
async fn telemetry_flush(state: State<'_, AppState>) -> Result<usize, String> {
    let settings = read_settings(&state)?;
    findex_core::telemetry::flush(&state.db_path, &settings)
        .await
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

fn source_preview(state: &AppState, request: SourceRequest) -> Result<SourcePreview, String> {
    let normalize = |value: &str| {
        let value = value.replace('\\', "/");
        if cfg!(windows) {
            value.to_ascii_lowercase()
        } else {
            value
        }
    };
    let indexed = state
        .storage
        .list_files()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|file| normalize(&file.path.to_string_lossy()) == normalize(&request.path))
        .ok_or_else(|| "source preview is restricted to exact indexed paths".to_string())?;
    let start = request.start_line.max(1);
    let requested_end = request.end_line.max(start);
    let end = requested_end.min(start.saturating_add(399));
    let reader = BufReader::new(fs::File::open(&indexed.path).map_err(|error| error.to_string())?);
    let mut text = String::new();
    let mut actual_end = start.saturating_sub(1);
    for (index, line) in reader.lines().enumerate() {
        let line_number = index + 1;
        if line_number > end {
            break;
        }
        if line_number >= start {
            text.push_str(&line.map_err(|error| error.to_string())?);
            text.push('\n');
            actual_end = line_number;
        }
    }
    Ok(SourcePreview {
        path: indexed.path.to_string_lossy().to_string(),
        start_line: start,
        end_line: actual_end,
        text,
        truncated: requested_end > end,
    })
}

async fn api_source(
    AxumState(state): AxumState<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SourceRequest>,
) -> Response {
    authorized_json(&state, &headers, || source_preview(&state, request))
}

async fn api_settings(AxumState(state): AxumState<Arc<AppState>>, headers: HeaderMap) -> Response {
    authorized_json(&state, &headers, || read_settings(&state))
}

async fn api_update_settings(
    AxumState(state): AxumState<Arc<AppState>>,
    headers: HeaderMap,
    Json(settings): Json<FindexSettings>,
) -> Response {
    authorized_json(&state, &headers, || persist_settings(&state, settings))
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

async fn serve_api(state: Arc<AppState>, listener: std::net::TcpListener) -> Result<(), String> {
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
        .route("/api/source", post(api_source))
        .route("/api/settings", get(api_settings).post(api_update_settings))
        .layer(cors)
        .with_state(state);
    let listener =
        tokio::net::TcpListener::from_std(listener).map_err(|error| error.to_string())?;
    axum::serve(listener, app)
        .await
        .map_err(|error| error.to_string())
}

fn main() {
    findex_core::runtime::configure_runtime();
    let db_override = std::env::var("FINDEX_DB_PATH").ok().map(PathBuf::from);
    let requested_port = std::env::var("FINDEX_DASHBOARD_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(37_421);
    let updater_plugin = findex_core::updater::updater_public_key()
        .map(|public_key| {
            tauri_plugin_updater::Builder::new()
                .pubkey(public_key)
                .build()
        })
        .unwrap_or_else(|| tauri_plugin_updater::Builder::new().build());
    tauri::Builder::default()
        // This must be the first plugin so Windows/Linux deep-link launches are
        // forwarded to the existing process instead of creating a second index owner.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        .plugin(updater_plugin)
        .plugin(tauri_plugin_deep_link::init())
        .manage(PendingDesktopUpdate::default())
        .manage(PendingDeepLink::default())
        .setup(move |app| {
            let db_path = match db_override.clone() {
                Some(path) => path,
                None => app.path().app_local_data_dir()?.join("index"),
            };
            fs::create_dir_all(&db_path)?;
            let listener = std::net::TcpListener::bind(("127.0.0.1", requested_port))
                .or_else(|error| {
                    eprintln!(
                        "Findex dashboard port {requested_port} is unavailable ({error}); using an ephemeral loopback port"
                    );
                    std::net::TcpListener::bind(("127.0.0.1", 0))
                })?;
            listener.set_nonblocking(true)?;
            let bind = listener.local_addr()?;
            let settings = findex_core::settings::load_or_default(&db_path);
            findex_core::runtime::apply_runtime_settings(&settings);
            let reranker = create_reranker();
            let embedder = create_embedder(128);
            findex_core::runtime::start_model_idle_janitor(&embedder, &reranker);
            let state = AppState {
                db_path: db_path.clone(),
                storage: Arc::new(Storage::open(&db_path)?),
                reranker,
                embedder,
                settings: Arc::new(RwLock::new(settings.clone())),
                api_url: format!("http://{bind}"),
                api_token: uuid::Uuid::new_v4().to_string(),
                quitting: Arc::new(AtomicBool::new(false)),
            };
            let _ = findex_core::telemetry::record_event(
                &state.db_path,
                &settings,
                Some(&state.storage),
                "desktop_started",
                json!({ "surface": "desktop" }),
            );
            findex_core::telemetry::install_panic_hook(
                state.db_path.clone(),
                state.settings.clone(),
            );
            let server_state = Arc::new(state.clone());
            let telemetry_state = state.clone();
            app.manage(state);
            setup_deep_links(app)?;
            setup_tray(app)?;
            tauri::async_runtime::spawn(async move {
                if let Err(error) = serve_api(server_state, listener).await {
                    eprintln!("Findex dashboard API stopped: {error}");
                }
            });
            tauri::async_runtime::spawn(async move {
                let settings = telemetry_state
                    .settings
                    .read()
                    .map(|settings| settings.clone())
                    .unwrap_or_default();
                if settings.telemetry.enabled {
                    if let Err(error) =
                        findex_core::telemetry::flush(&telemetry_state.db_path, &settings).await
                    {
                        eprintln!("telemetry queue preserved for retry: {error}");
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let Some(state) = window.app_handle().try_state::<AppState>() else {
                    return;
                };
                let minimize = state
                    .settings
                    .read()
                    .map(|settings| settings.ui.minimize_to_tray)
                    .unwrap_or(false);
                if minimize && !state.quitting.load(Ordering::Acquire) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_api_config,
            take_pending_deep_link,
            get_graph_data,
            get_architecture,
            search_symbols,
            get_stats,
            get_ast,
            run_graph_query,
            inspect_impact,
            reindex,
            get_settings,
            set_settings,
            auth_login,
            auth_status,
            auth_logout,
            telemetry_status,
            telemetry_flush,
            list_models,
            download_model_profile,
            check_for_update,
            install_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
