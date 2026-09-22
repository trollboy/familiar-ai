//! Embedded localhost-only dashboard server.
//!
//! Provides `/health`, `/stats`, `/projects`, `/recent` JSON endpoints and
//! a minimal HTML page at `/`. No auth, no framework, no SPA.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::net::TcpListener;

use familiar_ai_core::{AppStatus, BacklogDiscovery, FilesystemBacklogDiscovery, VersionInfo};
use familiar_ai_daemon::stewardship::StewardshipError;
use familiar_ai_llm::InferenceRouter;
use familiar_ai_storage::repos::stats;
use familiar_ai_storage::Database;

#[derive(Clone)]
pub struct DashboardState {
    pub db: Arc<Mutex<Database>>,
    pub status: Arc<Mutex<AppStatus>>,
    pub router: Arc<InferenceRouter>,
    pub start_time: DateTime<Utc>,
    /// PRD-108: same bounded reconcile-on-read fallback the tray/desktop
    /// transport uses, so the HTTP dashboard's backlog reads from the same
    /// reconciled view rather than an independently stale one.
    pub reconciler: Arc<familiar_ai_daemon::backlog_reconciler::BacklogReconciler>,
}

pub async fn run_dashboard(
    state: DashboardState,
    bind_addr: String,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let app = Router::new()
        .route("/", get(html_page))
        .route("/health", get(health))
        .route("/stats", get(stats_endpoint))
        .route("/projects", get(projects))
        .route("/recent", get(recent))
        .route("/stewardship/repositories", get(stewardship_repositories))
        .route("/stewardship/backlog", get(stewardship_backlog))
        .route("/stewardship/sessions", get(stewardship_sessions))
        .route(
            "/stewardship/sessions/{session_id}/attempts",
            get(stewardship_attempts),
        )
        .route(
            "/stewardship/sessions/{session_id}/budget",
            get(stewardship_budget),
        )
        .route(
            "/stewardship/sessions/{session_id}/review",
            get(stewardship_review),
        )
        .route("/stewardship/checkpoints", get(stewardship_checkpoints))
        .route("/stewardship/recovery", get(stewardship_recovery))
        .route("/stewardship/delivery", get(stewardship_delivery))
        .route("/stewardship/gates", get(stewardship_gates))
        .route(
            "/stewardship/reconciliation",
            get(stewardship_reconciliation),
        )
        .route("/favicon.png", get(favicon))
        .route("/settings/inference", get(inference_settings_page))
        .route("/settings/inference/status", get(inference_status))
        .route(
            "/settings/inference/test",
            axum::routing::post(inference_test),
        )
        .with_state(state);

    let listener = match TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, addr = %bind_addr, "failed to bind dashboard");
            return;
        }
    };

    tracing::info!(addr = %bind_addr, "dashboard started");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.changed().await;
        })
        .await
        .ok();

    tracing::info!("dashboard stopped");
}

async fn health(State(state): State<DashboardState>) -> impl IntoResponse {
    let version = VersionInfo::current();
    let uptime = (Utc::now() - state.start_time).num_seconds();

    let db_ok = {
        let db = state.db.lock().unwrap();
        db.conn().execute_batch("SELECT 1").is_ok()
    };

    let status = state.status.lock().unwrap().clone();
    let router_health = state.router.health().await;

    Json(json!({
        "daemon_uptime_secs": uptime,
        "version": version.version,
        "git_sha": version.git_sha,
        "build_date": version.build_date,
        "db_reachable": db_ok,
        "watcher_running": true,
        "active_projects": status.active_projects,
        "inference": {
            "text_mode": router_health.text_mode,
            "text_primary": router_health.text_primary,
            "text_fallback": router_health.text_fallback,
            "embedding_primary": router_health.embedding_primary,
            "embedding_fallback": router_health.embedding_fallback,
        }
    }))
}

async fn stats_endpoint(State(state): State<DashboardState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    match stats::global_stats(&db) {
        Ok(s) => Json(json!({
            "projects": s.projects,
            "active_projects": s.active_projects,
            "file_summaries": s.file_summaries,
            "decisions": s.decisions,
            "session_rollups": s.session_rollups,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn projects(State(state): State<DashboardState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    match stats::projects_with_counts(&db) {
        Ok(projects) => {
            let list: Vec<_> = projects
                .into_iter()
                .map(|p| {
                    json!({
                        "id": p.id,
                        "name": p.name,
                        "repo_root": p.repo_root,
                        "active": p.active,
                        "last_used_at": p.last_used_at,
                        "file_summaries": p.file_summaries,
                        "decisions": p.decisions,
                        "session_rollups": p.session_rollups,
                    })
                })
                .collect();
            Json(json!({"projects": list})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn recent(State(state): State<DashboardState>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let summaries = stats::recent_file_summaries(&db, 10).unwrap_or_default();
    let decisions = stats::recent_decisions(&db, 10).unwrap_or_default();
    let rollups = stats::recent_session_rollups(&db, 5).unwrap_or_default();

    Json(json!({
        "recent_summaries": summaries,
        "recent_decisions": decisions,
        "recent_rollups": rollups,
    }))
}

/// Resolves the repository identity for a stewardship request from its
/// mandatory `repo` query parameter. There is no cwd-based default here —
/// unlike the CLI and MCP, the dashboard is one long-running process that
/// may serve many repositories, so the caller must always say which one.
fn resolve_repo_identity(
    params: &HashMap<String, String>,
) -> Result<familiar_ai_core::RepositoryIdentity, Box<Response>> {
    let Some(repo) = params.get("repo") else {
        return Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "missing required query parameter: repo"})),
            )
                .into_response(),
        ));
    };
    FilesystemBacklogDiscovery
        .resolve(std::path::Path::new(repo))
        .map_err(|e| {
            Box::new(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response(),
            )
        })
}

fn parse_limit(params: &HashMap<String, String>) -> usize {
    params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20)
}

fn stewardship_error_response(error: StewardshipError) -> Response {
    match error {
        StewardshipError::NotFound(message) => {
            (StatusCode::NOT_FOUND, Json(json!({"error": message}))).into_response()
        }
        StewardshipError::Storage(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": message})),
        )
            .into_response(),
    }
}

async fn stewardship_repositories(State(state): State<DashboardState>) -> Response {
    let db = state.db.lock().unwrap();
    match familiar_ai_daemon::stewardship::list_repositories(&db) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn stewardship_backlog(
    State(state): State<DashboardState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    // PRD-108: repair whatever the watcher missed before reading, bounded so
    // repeated polling of this route cannot become a continuous full scan.
    if let Some(repo) = params.get("repo") {
        state
            .reconciler
            .reconcile_if_stale(std::path::Path::new(repo));
    }
    let db = state.db.lock().unwrap();
    let status = params.get("status").map(String::as_str);
    let cursor = params.get("cursor").map(String::as_str);
    // PRD-109: the loopback dashboard has no repository configuration in
    // hand, so its rows derive the lifecycle from the ledger, the latest
    // attempt and the checkpoint; the file's own status rides on the
    // operator transport the desktop and tray use.
    match familiar_ai_daemon::stewardship::list_backlog(
        &db,
        &identity,
        None,
        status,
        cursor,
        parse_limit(&params),
    ) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn stewardship_sessions(
    State(state): State<DashboardState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    let db = state.db.lock().unwrap();
    let cursor = params.get("cursor").map(String::as_str);
    match familiar_ai_daemon::stewardship::list_sessions(
        &db,
        &identity,
        cursor,
        parse_limit(&params),
    ) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn stewardship_attempts(
    State(state): State<DashboardState>,
    Path(session_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    let db = state.db.lock().unwrap();
    let cursor = params.get("cursor").and_then(|s| s.parse::<i64>().ok());
    match familiar_ai_daemon::stewardship::list_attempts(
        &db,
        &identity,
        &session_id,
        cursor,
        parse_limit(&params),
    ) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn stewardship_checkpoints(
    State(state): State<DashboardState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    let db = state.db.lock().unwrap();
    let cursor = params.get("cursor").map(String::as_str);
    match familiar_ai_daemon::stewardship::list_checkpoints(
        &db,
        &identity,
        cursor,
        parse_limit(&params),
    ) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn stewardship_recovery(
    State(state): State<DashboardState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    let db = state.db.lock().unwrap();
    let cursor = params.get("cursor").and_then(|s| s.parse::<i64>().ok());
    match familiar_ai_daemon::stewardship::list_recovery_events(
        &db,
        &identity,
        cursor,
        parse_limit(&params),
    ) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn stewardship_delivery(
    State(state): State<DashboardState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    let db = state.db.lock().unwrap();
    let cursor = params.get("cursor").map(String::as_str);
    match familiar_ai_daemon::stewardship::list_delivery_decisions(
        &db,
        &identity,
        cursor,
        parse_limit(&params),
    ) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn stewardship_budget(
    State(state): State<DashboardState>,
    Path(session_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    let db = state.db.lock().unwrap();
    match familiar_ai_daemon::stewardship::get_budget(&db, &identity, &session_id) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn stewardship_review(
    State(state): State<DashboardState>,
    Path(session_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    let db = state.db.lock().unwrap();
    match familiar_ai_daemon::stewardship::list_review_findings(&db, &identity, &session_id) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn stewardship_gates(
    State(state): State<DashboardState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    let db = state.db.lock().unwrap();
    match familiar_ai_daemon::stewardship::list_pending_human_gates(
        &db,
        &identity,
        parse_limit(&params),
    ) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn stewardship_reconciliation(
    State(state): State<DashboardState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    let (Some(start), Some(end)) = (params.get("start"), params.get("end")) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing required query parameters: start, end"})),
        )
            .into_response();
    };
    let db = state.db.lock().unwrap();
    match familiar_ai_daemon::stewardship::get_reconciliation(&db, &identity, start, end) {
        Ok(value) => Json(value).into_response(),
        Err(error) => stewardship_error_response(error),
    }
}

async fn inference_status(State(state): State<DashboardState>) -> impl IntoResponse {
    let health = state.router.health().await;
    Json(json!(health))
}

async fn inference_test(
    State(state): State<DashboardState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let target = body
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("text_primary");

    let result = state.router.test_connection(target).await;
    Json(json!(result))
}

async fn inference_settings_page() -> Html<&'static str> {
    Html(SETTINGS_HTML)
}

const SETTINGS_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<link rel="icon" type="image/png" href="/favicon.png">
<title>Familiar — Inference Settings</title>
<style>
  body { font-family: system-ui, sans-serif; max-width: 700px; margin: 2rem auto; padding: 0 1rem; background: #fafafa; color: #222; }
  h1 { border-bottom: 2px solid #333; padding-bottom: 0.5rem; }
  h2 { margin-top: 2rem; color: #555; }
  .card { background: #fff; border: 1px solid #ddd; border-radius: 6px; padding: 1rem; margin: 0.5rem 0; }
  .ok { color: #16a34a; } .err { color: #dc2626; } .warn { color: #d97706; }
  .mono { font-family: monospace; font-size: 0.85rem; }
  button { padding: 0.4rem 1rem; border: 1px solid #999; border-radius: 4px; cursor: pointer; margin: 0.3rem 0; }
  button:hover { background: #eee; }
  .status-row { display: flex; align-items: center; gap: 1rem; margin: 0.3rem 0; }
  .badge { display: inline-block; padding: 0.1rem 0.5rem; border-radius: 4px; font-size: 0.8rem; }
  .badge.ok { background: #dcfce7; } .badge.err { background: #fee2e2; } .badge.warn { background: #fef3c7; }
  small { color: #888; }
  a { color: #2563eb; }
</style>
</head>
<body>
<h1><a href="/">Familiar</a> — Inference Settings</h1>
<p><small>Changes made here are in-memory only and reset on restart. Edit <code>config.toml</code> for persistent changes.</small></p>
<div id="content">Loading…</div>
<script>
async function load() {
  try {
    const status = await fetch('/settings/inference/status').then(r=>r.json());
    render(status);
  } catch(e) {
    document.getElementById('content').textContent = 'Failed: ' + e;
  }
}
function badge(h) {
  if (!h) return '<span class="badge err">not configured</span>';
  if (h.healthy) return '<span class="badge ok">healthy</span>';
  if (h.loaded) return '<span class="badge warn">degraded</span>';
  return '<span class="badge err">not loaded</span>';
}
function render(s) {
  let html = '';
  html += `<h2>Text Inference</h2><div class="card">`;
  html += `<div class="status-row"><strong>Mode:</strong> ${s.text_mode}</div>`;
  html += `<div class="status-row"><strong>Primary:</strong> ${badge(s.text_primary)} ${s.text_primary?.backend_name||''}</div>`;
  if (s.text_primary?.last_error) html += `<div class="err"><small>${s.text_primary.last_error}</small></div>`;
  html += `<button onclick="testConn('text_primary')">Test Primary</button>`;
  if (s.text_fallback) {
    html += `<div class="status-row"><strong>Fallback:</strong> ${badge(s.text_fallback)} ${s.text_fallback?.backend_name||''}</div>`;
    if (s.text_fallback?.last_error) html += `<div class="err"><small>${s.text_fallback.last_error}</small></div>`;
    html += `<button onclick="testConn('text_fallback')">Test Fallback</button>`;
  }
  html += `</div>`;

  html += `<h2>Embedding Inference</h2><div class="card">`;
  html += `<div class="status-row"><strong>Primary:</strong> ${badge(s.embedding_primary)} ${s.embedding_primary?.backend_name||''}</div>`;
  if (s.embedding_primary?.last_error) html += `<div class="err"><small>${s.embedding_primary.last_error}</small></div>`;
  html += `<button onclick="testConn('embed_primary')">Test Primary</button>`;
  if (s.embedding_fallback) {
    html += `<div class="status-row"><strong>Fallback:</strong> ${badge(s.embedding_fallback)} ${s.embedding_fallback?.backend_name||''}</div>`;
    html += `<button onclick="testConn('embed_fallback')">Test Fallback</button>`;
  }
  html += `</div>`;

  html += `<div id="test-result"></div>`;
  document.getElementById('content').innerHTML = html;
}
async function testConn(target) {
  const el = document.getElementById('test-result');
  el.innerHTML = '<div class="card">Testing…</div>';
  try {
    const r = await fetch('/settings/inference/test', {
      method: 'POST', headers: {'Content-Type':'application/json'},
      body: JSON.stringify({target})
    }).then(r=>r.json());
    let cls = r.connected ? 'ok' : 'err';
    el.innerHTML = `<div class="card"><strong>${target}:</strong> <span class="${cls}">${r.status_text}</span>
      ${r.backend_name?' ('+r.backend_name+')':''}
      ${r.latency_ms!=null?' — '+r.latency_ms+'ms':''}
      ${r.last_error?'<br><small class="err">'+r.last_error+'</small>':''}
    </div>`;
  } catch(e) { el.innerHTML = `<div class="card err">Test failed: ${e}</div>`; }
}
load();
</script>
</body>
</html>"#;

async fn favicon() -> impl IntoResponse {
    const ICON_BYTES: &[u8] = include_bytes!("../../familiar-ai-tray/assets/icon.png");
    (
        [(axum::http::header::CONTENT_TYPE, "image/png")],
        ICON_BYTES,
    )
}

async fn html_page() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<link rel="icon" type="image/png" href="/favicon.png">
<title>Familiar Dashboard</title>
<style>
  body { font-family: system-ui, sans-serif; max-width: 1000px; margin: 2rem auto; padding: 0 1rem; background: #fafafa; color: #222; }
  h1 { border-bottom: 2px solid #333; padding-bottom: 0.5rem; }
  h2 { margin-top: 2rem; color: #555; }
  h3 { margin: 1rem 0 0.4rem; font-size: 1rem; color: #555; }
  .card { background: #fff; border: 1px solid #ddd; border-radius: 6px; padding: 1rem; margin: 0.5rem 0; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 0.5rem; }
  .stat { text-align: center; }
  .stat .num { font-size: 2rem; font-weight: bold; color: #2563eb; }
  .stat .label { font-size: 0.85rem; color: #777; }
  table { border-collapse: collapse; width: 100%; }
  th, td { text-align: left; padding: 0.4rem 0.6rem; border-bottom: 1px solid #eee; vertical-align: top; }
  th { background: #f5f5f5; font-weight: 600; }
  .ok { color: #16a34a; } .err { color: #dc2626; } .warn { color: #d97706; }
  .mono { font-family: monospace; font-size: 0.85rem; }
  .muted { color: #777; }
  .nowrap { white-space: nowrap; }
  .attention { border-left: 4px solid #d97706; }
  .attention h3 { margin-top: 0; color: #d97706; }
  .pill { display: inline-block; padding: 0.05rem 0.45rem; border-radius: 10px; font-size: 0.75rem; background: #eef; color: #334; }
  .pill.bad { background: #fde8e8; color: #a11; }
  .pill.good { background: #e7f6ec; color: #161; }
  button { font: inherit; padding: 0.25rem 0.7rem; border: 1px solid #bbb; border-radius: 4px; background: #fff; cursor: pointer; }
  button:hover { background: #f0f0f0; }
  select { font: inherit; padding: 0.25rem; }
  pre { background: #f7f7f7; padding: 0.5rem; border-radius: 4px; overflow-x: auto; font-size: 0.8rem; margin: 0.3rem 0; }
  #loading { text-align: center; padding: 2rem; color: #999; }
</style>
</head>
<body>
<h1>Familiar</h1>
<div id="loading">Loading&hellip;</div>
<div id="content" style="display:none">
  <div id="health-section"></div>

  <h2>Stewardship</h2>
  <div class="card">
    Repository:
    <select id="repo-select"><option>loading&hellip;</option></select>
    <span id="repo-note" class="muted"></span>
  </div>
  <div id="gates-section"></div>
  <div id="backlog-section"></div>
  <div id="sessions-section"></div>

  <h2>Stats</h2>
  <div id="stats-section" class="grid"></div>
  <h2>Projects</h2>
  <div id="projects-section"></div>
  <h2>Recent Activity</h2>
  <div id="recent-section"></div>
</div>
<script>
// Paths, reasons and agent-written detail strings all land in innerHTML, and
// some of them are model output. Escape everything that is not markup we wrote.
function esc(v) {
  if (v === null || v === undefined) return '';
  return String(v).replace(/[&<>"']/g, c => ({
    '&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'
  })[c]);
}
function usd(micro) {
  if (micro === null || micro === undefined) return '&mdash;';
  return '$' + (micro / 1e6).toFixed(2);
}
function mins(ms) {
  if (!ms) return '&mdash;';
  const m = Math.floor(ms / 60000);
  return m >= 60 ? Math.floor(m/60) + 'h ' + (m%60) + 'm' : m + 'm';
}
// A lockfile finding can enumerate every new crate in the graph, which is
// hundreds of names. Left whole it makes one table row taller than the page.
function clip(v, max) {
  const s = String(v === null || v === undefined ? '' : v);
  max = max || 200;
  return s.length > max
    ? '<span title="' + esc(s) + '">' + esc(s.slice(0, max)) + '&hellip;</span>'
    : esc(s);
}
function when(ts) {
  if (!ts) return '&mdash;';
  const d = new Date(ts);
  return isNaN(d) ? esc(ts) : d.toLocaleString();
}
async function getJSON(url) {
  const r = await fetch(url);
  const body = await r.json();
  if (!r.ok) throw new Error(body.error || ('HTTP ' + r.status));
  return body;
}

async function load() {
  try {
    const [health, stats, projects, recent] = await Promise.all([
      getJSON('/health'), getJSON('/stats'), getJSON('/projects'), getJSON('/recent'),
    ]);
    document.getElementById('loading').style.display = 'none';
    document.getElementById('content').style.display = 'block';
    renderHealth(health);
    renderStats(stats);
    renderProjects(projects.projects || []);
    renderRecent(recent);
    loadRepositories();
  } catch(e) {
    document.getElementById('loading').textContent = 'Failed to load: ' + e;
  }
}

function renderHealth(h) {
  // The health endpoint reports `inference`, not `llm`. This card read `h.llm`
  // for long enough that it showed "off" no matter what the router was doing.
  const inf = h.inference || {};
  const text = inf.text_primary || {};
  const state = text.loaded ? (text.healthy ? 'healthy' : 'degraded') : 'off';
  const cls = text.loaded ? (text.healthy ? 'ok' : 'warn') : 'muted';
  const el = document.getElementById('health-section');
  el.innerHTML = '<div class="card"><strong>Daemon</strong> v' + esc(h.version || '?') +
    ' (' + esc(h.git_sha || '?') + ') &middot; uptime ' + Math.floor((h.daemon_uptime_secs||0)/60) + 'm' +
    ' &middot; DB: <span class="' + (h.db_reachable?'ok':'err') + '">' + (h.db_reachable?'OK':'unreachable') + '</span>' +
    ' &middot; inference: <span class="' + cls + '">' + esc(state) + '</span>' +
    ' <span class="muted">(mode ' + esc(inf.text_mode || 'unknown') +
    (text.backend_name ? ', ' + esc(text.backend_name) : '') + ')</span>' +
    (text.last_error ? '<br><small class="err">' + esc(text.last_error) + '</small>' : '') +
    '</div>';
}
function renderStats(s) {
  const items = [['Projects', s.projects], ['Active', s.active_projects],
    ['Summaries', s.file_summaries], ['Decisions', s.decisions], ['Rollups', s.session_rollups]];
  document.getElementById('stats-section').innerHTML = items.map(
    ([l,n]) => '<div class="card stat"><div class="num">' + esc(n) + '</div><div class="label">' + esc(l) + '</div></div>'
  ).join('');
}
function renderProjects(ps) {
  const el = document.getElementById('projects-section');
  if(!ps.length) { el.innerHTML = '<div class="card muted">No watched projects. The watcher is a wave-one surface and is separate from the stewardship state above.</div>'; return; }
  el.innerHTML = '<table><tr><th>Name</th><th>Root</th><th>Files</th><th>Decisions</th><th>Rollups</th></tr>' +
    ps.map(p => '<tr><td>' + esc(p.name) + '</td><td class="mono">' + esc(p.repo_root) + '</td><td>' +
      esc(p.file_summaries) + '</td><td>' + esc(p.decisions) + '</td><td>' + esc(p.session_rollups) + '</td></tr>').join('') +
    '</table>';
}
function renderRecent(r) {
  const el = document.getElementById('recent-section');
  let html = '';
  if(r.recent_summaries && r.recent_summaries.length) {
    html += '<h3>File Summaries</h3><table><tr><th>Path</th><th>Summary</th></tr>' +
      r.recent_summaries.slice(0,5).map(s => '<tr><td class="mono">' + esc(s.path) + '</td><td>' + esc(s.summary.slice(0,120)) + '</td></tr>').join('') + '</table>';
  }
  if(r.recent_decisions && r.recent_decisions.length) {
    html += '<h3>Decisions</h3><table><tr><th>Title</th><th>Summary</th></tr>' +
      r.recent_decisions.slice(0,5).map(d => '<tr><td>' + esc(d.title) + '</td><td>' + esc(d.summary.slice(0,120)) + '</td></tr>').join('') + '</table>';
  }
  if(r.recent_rollups && r.recent_rollups.length) {
    html += '<h3>Session Rollups</h3><table><tr><th>Summary</th></tr>' +
      r.recent_rollups.slice(0,5).map(ro => '<tr><td>' + esc(ro.summary.slice(0,200)) + '</td></tr>').join('') + '</table>';
  }
  if(!html) html = '<div class="card muted">No recent activity.</div>';
  el.innerHTML = html;
}

// ---- stewardship -------------------------------------------------------
// Every /stewardship route except /repositories is repository-scoped and
// takes a mandatory `repo`, so the picker below is what makes the rest of
// this section askable at all.
let currentRepo = null;

async function loadRepositories() {
  const sel = document.getElementById('repo-select');
  try {
    const data = await getJSON('/stewardship/repositories');
    const repos = data.repositories || [];
    if(!repos.length) {
      sel.innerHTML = '<option>none</option>';
      document.getElementById('repo-note').textContent = ' — no repository has stewardship state yet.';
      return;
    }
    sel.innerHTML = repos.map(r => '<option value="' + esc(r.path) + '">' + esc(r.path) + '</option>').join('');
    sel.onchange = () => selectRepo(sel.value);
    selectRepo(repos[0].path);
  } catch(e) {
    sel.innerHTML = '<option>error</option>';
    document.getElementById('repo-note').textContent = ' — ' + e;
  }
}

function selectRepo(path) {
  currentRepo = path;
  loadGates(); loadBacklog(); loadSessions();
}
function q(p) { return '?repo=' + encodeURIComponent(currentRepo) + (p || ''); }

async function loadGates() {
  const el = document.getElementById('gates-section');
  el.innerHTML = '<div class="card muted">Loading gates&hellip;</div>';
  try {
    const data = await getJSON('/stewardship/gates' + q(''));
    const items = data.items || [];
    if(!items.length) {
      el.innerHTML = '<div class="card"><h3>Waiting on you</h3><span class="ok">Nothing is blocked.</span></div>';
      return;
    }
    // One PRD can stop several times for different reasons. Group by PRD so
    // this reads as "what is stuck" rather than "how many times it stuck",
    // and keep the recovery commands folded away — printed in full they run
    // to several screens and push the rest of the page out of sight.
    const groups = new Map();
    items.forEach(g => {
      const key = g.prd_path || g.prd_id;
      if(!groups.has(key)) groups.set(key, { prd_id: g.prd_id, prd_path: g.prd_path, details: [], commands: [] });
      const grp = groups.get(key);
      const d = g.detail || g.kind;
      if(!grp.details.includes(d)) grp.details.push(d);
      (g.recovery_commands || []).forEach(c => { if(!grp.commands.includes(c)) grp.commands.push(c); });
    });
    const rows = Array.from(groups.values());
    el.innerHTML = '<div class="card attention"><h3>Waiting on you &mdash; ' +
      rows.length + ' PRD' + (rows.length === 1 ? '' : 's') + ', ' +
      items.length + ' stopped attempt' + (items.length === 1 ? '' : 's') + '</h3>' +
      '<table><tr><th>PRD</th><th>Why it stopped</th><th></th></tr>' +
      rows.map((g,i) =>
        '<tr><td><strong>' + esc(g.prd_id || '') + '</strong><br>' +
        '<span class="mono muted nowrap">' + esc(g.prd_path) + '</span></td>' +
        '<td>' + g.details.map(d => '<span class="pill bad">' + esc(d) + '</span>').join(' ') + '</td>' +
        '<td><button onclick="toggleGate(' + i + ')">how to clear</button></td></tr>' +
        '<tr><td colspan="3" id="gate-' + i + '" style="display:none"><pre>' +
        g.commands.map(esc).join('\n') + '</pre></td></tr>').join('') +
      '</table></div>';
  } catch(e) { el.innerHTML = '<div class="card err">Gates failed: ' + esc(e.message) + '</div>'; }
}

function toggleGate(i) {
  const c = document.getElementById('gate-' + i);
  c.style.display = c.style.display === 'none' ? 'table-cell' : 'none';
}

async function loadBacklog() {
  const el = document.getElementById('backlog-section');
  el.innerHTML = '<div class="card muted">Loading backlog&hellip;</div>';
  try {
    // One page is a sample, not the whole backlog; say so rather than imply a total.
    const data = await getJSON('/stewardship/backlog' + q('&limit=200'));
    const items = data.items || [];
    const counts = {};
    items.forEach(i => { counts[i.status] = (counts[i.status] || 0) + 1; });
    const open = items.filter(i => i.status !== 'completed');
    el.innerHTML = '<div class="card"><h3>Backlog</h3>' +
      (Object.keys(counts).length
        ? Object.entries(counts).map(([s,n]) =>
            '<span class="pill ' + (s === 'completed' ? 'good' : '') + '">' + esc(s) + ': ' + n + '</span> ').join('')
        : '<span class="muted">empty</span>') +
      (data.next_cursor ? ' <span class="muted">(first 200 shown)</span>' : '') +
      (open.length
        ? '<table style="margin-top:0.6rem"><tr><th>PRD</th><th>Status</th><th>Updated</th></tr>' +
          open.slice(0,25).map(i => '<tr><td class="mono">' + esc(i.prd_path) + '</td><td>' + esc(i.status) +
            '</td><td class="muted">' + when(i.updated_at) + '</td></tr>').join('') + '</table>'
        : '<div class="muted" style="margin-top:0.5rem">Nothing open.</div>') +
      '</div>';
  } catch(e) { el.innerHTML = '<div class="card err">Backlog failed: ' + esc(e.message) + '</div>'; }
}

async function loadSessions() {
  const el = document.getElementById('sessions-section');
  el.innerHTML = '<div class="card muted">Loading sessions&hellip;</div>';
  try {
    const data = await getJSON('/stewardship/sessions' + q('&limit=10'));
    const items = data.items || [];
    if(!items.length) { el.innerHTML = '<div class="card"><h3>Sessions</h3><span class="muted">No drive sessions yet.</span></div>'; return; }
    el.innerHTML = '<div class="card"><h3>Sessions</h3><table>' +
      '<tr><th>Session</th><th>Started</th><th>Ended</th><th></th></tr>' +
      items.map((s,i) =>
        '<tr><td class="mono">' + esc(s.session_id.slice(0,28)) + '</td>' +
        '<td class="muted">' + when(s.started_at) + '</td>' +
        '<td class="muted">' + (s.ended_at ? when(s.ended_at) : '<span class="warn">running</span>') + '</td>' +
        '<td><button onclick="toggleSession(' + i + ', \'' + esc(s.session_id) + '\')">details</button></td></tr>' +
        '<tr><td colspan="4" id="sess-' + i + '" style="display:none"></td></tr>').join('') +
      '</table></div>';
  } catch(e) { el.innerHTML = '<div class="card err">Sessions failed: ' + esc(e.message) + '</div>'; }
}

async function toggleSession(i, sessionId) {
  const cell = document.getElementById('sess-' + i);
  if(cell.style.display !== 'none') { cell.style.display = 'none'; return; }
  cell.style.display = 'table-cell';
  cell.innerHTML = '<span class="muted">Loading&hellip;</span>';
  const base = '/stewardship/sessions/' + encodeURIComponent(sessionId);
  try {
    const [attempts, budget, review] = await Promise.all([
      getJSON(base + '/attempts' + q('&limit=50')),
      getJSON(base + '/budget' + q('')),
      getJSON(base + '/review' + q('&limit=50')),
    ]);
    const w = budget.warrant || {};
    let html = '<strong>Budget</strong><br>' +
      '<span class="pill">spent ' + usd(budget.known_cost_microusd) + '</span> ' +
      '<span class="pill">' + esc(budget.known_cost_attempts) + ' priced attempts</span> ' +
      (budget.unknown_cost_attempts ? '<span class="pill bad">' + esc(budget.unknown_cost_attempts) + ' unpriced</span> ' : '') +
      '<span class="pill">warrant: ' + esc(w.max_prds) + ' PRDs / ' + mins(w.max_duration_ms) + '</span>';
    if(w.prd_allowlist && w.prd_allowlist.length) {
      html += ' <span class="muted mono">[' + w.prd_allowlist.map(esc).join(', ') + ']</span>';
    }

    const dispositions = {};
    (review.items || []).forEach(r => { dispositions[r.prd_id] = r; });

    html += '<br><strong style="display:inline-block;margin-top:0.6rem">Attempts</strong>' +
      '<table><tr><th>#</th><th>PRD</th><th>Model</th><th>Outcome</th><th>Review</th><th>Cost</th><th>Took</th></tr>' +
      (attempts.items || []).map(a => {
        const rev = dispositions[a.prd_id];
        const bad = a.outcome !== 'delivered' && a.outcome !== 'completed';
        return '<tr><td>' + esc(a.sequence) + '</td><td class="mono">' + esc(a.prd_id) + '</td>' +
          '<td>' + esc(a.model || '') + '</td>' +
          '<td><span class="pill ' + (bad ? 'bad' : 'good') + '">' + esc(a.outcome) + '</span>' +
          (a.retained_reason ? '<br><small class="muted">' + esc(a.retained_reason) + '</small>' : '') +
          (a.retained_detail ? '<br><small class="muted" title="' + esc(a.retained_detail) + '">' + esc(a.retained_detail.slice(0, 160)) + '</small>' : '') + '</td>' +
          '<td>' + (rev ? esc(rev.disposition) +
            (rev.blocking_findings && rev.blocking_findings.length
              ? '<br><small class="err">' + rev.blocking_findings.length + ' blocking</small>' : '')
            : '<span class="muted">&mdash;</span>') + '</td>' +
          '<td>' + usd(a.known_cost_microusd) + '</td><td class="muted">' + mins(a.duration_ms) + '</td></tr>';
      }).join('') + '</table>';

    const blocking = (review.items || []).flatMap(r => (r.blocking_findings || []).map(f => [r.prd_id, f]));
    if(blocking.length) {
      html += '<strong style="display:inline-block;margin-top:0.6rem">Blocking review findings</strong>' +
        '<table><tr><th>PRD</th><th>Path</th><th>Rule</th><th>Detail</th></tr>' +
        blocking.slice(0,25).map(([prd,f]) =>
          '<tr><td class="mono">' + esc(prd) + '</td><td class="mono">' + esc(f.path) + '</td>' +
          '<td>' + esc(f.rule_id || f.decision) + '</td><td class="muted">' + clip(f.rule_detail) + '</td></tr>').join('') +
        '</table>';
    }
    cell.innerHTML = html;
  } catch(e) { cell.innerHTML = '<span class="err">Failed: ' + esc(e.message) + '</span>'; }
}

load();
</script>
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use familiar_ai_core::config::InferenceConfig;
    use familiar_ai_core::models::NewProject;
    use familiar_ai_core::BacklogStatusStore;
    use familiar_ai_storage::ProjectRepository;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn make_state() -> DashboardState {
        let db = Arc::new(Mutex::new(Database::open_in_memory().unwrap()));
        db.lock().unwrap().run_migrations().unwrap();
        DashboardState {
            db: db.clone(),
            status: Arc::new(Mutex::new(AppStatus::new())),
            router: Arc::new(InferenceRouter::new(&InferenceConfig::default())),
            start_time: Utc::now(),
            reconciler: familiar_ai_daemon::backlog_reconciler::BacklogReconciler::new(
                db,
                familiar_ai_core::Config::default(),
                std::time::Duration::from_millis(50),
                std::time::Duration::from_secs(3),
            ),
        }
    }

    fn make_app(state: DashboardState) -> Router {
        Router::new()
            .route("/", get(html_page))
            .route("/health", get(health))
            .route("/stats", get(stats_endpoint))
            .route("/projects", get(projects))
            .route("/recent", get(recent))
            .route("/stewardship/repositories", get(stewardship_repositories))
            .route("/stewardship/backlog", get(stewardship_backlog))
            .route("/stewardship/sessions", get(stewardship_sessions))
            .route(
                "/stewardship/sessions/{session_id}/attempts",
                get(stewardship_attempts),
            )
            .route(
                "/stewardship/sessions/{session_id}/budget",
                get(stewardship_budget),
            )
            .route(
                "/stewardship/sessions/{session_id}/review",
                get(stewardship_review),
            )
            .route("/stewardship/checkpoints", get(stewardship_checkpoints))
            .route("/stewardship/recovery", get(stewardship_recovery))
            .route("/stewardship/delivery", get(stewardship_delivery))
            .route("/stewardship/gates", get(stewardship_gates))
            .route(
                "/stewardship/reconciliation",
                get(stewardship_reconciliation),
            )
            .with_state(state)
    }

    async fn get_json(app: &Router, path: &str) -> serde_json::Value {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn health_returns_200() {
        let state = make_state();
        let app = make_app(state);
        let json = get_json(&app, "/health").await;
        assert!(json["daemon_uptime_secs"].is_number());
        assert!(json["db_reachable"].as_bool().unwrap());
        assert!(json["version"].is_string());
    }

    #[tokio::test]
    async fn stats_returns_200_with_counts() {
        let state = make_state();
        let app = make_app(state);
        let json = get_json(&app, "/stats").await;
        assert_eq!(json["projects"], 0);
        assert_eq!(json["file_summaries"], 0);
    }

    #[tokio::test]
    async fn projects_returns_empty_list() {
        let state = make_state();
        let app = make_app(state);
        let json = get_json(&app, "/projects").await;
        assert!(json["projects"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn projects_returns_data_after_insert() {
        let state = make_state();
        {
            let db = state.db.lock().unwrap();
            db.create_project(&NewProject {
                name: "test".into(),
                repo_root: "/test".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap();
        }
        let app = make_app(state);
        let json = get_json(&app, "/projects").await;
        let projects = json["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0]["name"], "test");
    }

    #[tokio::test]
    async fn recent_returns_200() {
        let state = make_state();
        let app = make_app(state);
        let json = get_json(&app, "/recent").await;
        assert!(json["recent_summaries"].is_array());
        assert!(json["recent_decisions"].is_array());
        assert!(json["recent_rollups"].is_array());
    }

    #[tokio::test]
    async fn html_page_returns_html() {
        let state = make_state();
        let app = make_app(state);
        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("text/html"));
    }

    fn temp_git_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        repo
    }

    async fn request_json(app: &Router, path: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn stewardship_backlog_requires_repo_query_param() {
        let state = make_state();
        let app = make_app(state);
        let (status, json) = request_json(&app, "/stewardship/backlog").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("repo"));
    }

    /// Without this route the dashboard cannot ask its own first question:
    /// every other stewardship endpoint demands a `repo` the caller has no
    /// way to discover.
    #[tokio::test]
    async fn stewardship_repositories_lists_known_repositories() {
        let repo = temp_git_repo();
        let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
        let state = make_state();
        {
            let mut db = state.db.lock().unwrap();
            let discovered = vec![familiar_ai_core::DiscoveredPrd {
                id: familiar_ai_core::PrdId::new(1),
                number: 1,
                path: familiar_ai_core::RepositoryPath::new("docs/prds/PRD-1.md").unwrap(),
                location: familiar_ai_core::PrdLocation::Active,
                title: "One".into(),
                dependencies: vec![],
                metadata: familiar_ai_core::PrdMetadata::default(),
                content_hash: "hash".into(),
            }];
            familiar_ai_storage::SqliteBacklogRepository::new(db.conn_mut())
                .reconcile_and_snapshot(&identity, &discovered)
                .unwrap();
        }
        let app = make_app(state);
        let (status, json) = request_json(&app, "/stewardship/repositories").await;
        assert_eq!(status, StatusCode::OK);
        let items = json["repositories"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["repository_key"], identity.key);
        // The path form is what the caller must hand back as `repo`, so the
        // stored git-directory key is not good enough on its own.
        assert_eq!(
            items[0]["path"],
            identity.key.strip_suffix("/.git").unwrap()
        );
    }

    #[tokio::test]
    async fn stewardship_backlog_reflects_reconciled_state() {
        let repo = temp_git_repo();
        let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
        let state = make_state();
        {
            let mut db = state.db.lock().unwrap();
            let discovered = vec![familiar_ai_core::DiscoveredPrd {
                id: familiar_ai_core::PrdId::new(1),
                number: 1,
                path: familiar_ai_core::RepositoryPath::new("docs/prds/PRD-1.md").unwrap(),
                location: familiar_ai_core::PrdLocation::Active,
                title: "One".into(),
                dependencies: vec![],
                metadata: familiar_ai_core::PrdMetadata::default(),
                content_hash: "hash".into(),
            }];
            familiar_ai_storage::SqliteBacklogRepository::new(db.conn_mut())
                .reconcile_and_snapshot(&identity, &discovered)
                .unwrap();
        }
        let app = make_app(state);
        let path = format!("/stewardship/backlog?repo={}", repo.path().display());
        let (status, json) = request_json(&app, &path).await;
        assert_eq!(status, StatusCode::OK);
        let items = json["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["prd_path"], "docs/prds/PRD-1.md");
        assert_eq!(json["repository_key"], identity.key);
    }

    #[tokio::test]
    async fn stewardship_sessions_attempts_and_budget_agree() {
        let repo = temp_git_repo();
        let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
        let state = make_state();
        {
            let db = state.db.lock().unwrap();
            let driver = familiar_ai_storage::DriverRepository::new(db.conn());
            driver
                .open_session("session-1", &identity.key, r#"{"max_prds":1}"#)
                .unwrap();
            let a = driver
                .record_attempt_started("session-1", "PRD-1", "docs/prds/PRD-1.md", Some("exec-1"))
                .unwrap();
            driver
                .record_attempt_finished("session-1", a, "completed", None, Some(1_000), Some(10))
                .unwrap();
        }
        let app = make_app(state);

        let sessions_path = format!("/stewardship/sessions?repo={}", repo.path().display());
        let (status, json) = request_json(&app, &sessions_path).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["items"].as_array().unwrap().len(), 1);

        let attempts_path = format!(
            "/stewardship/sessions/session-1/attempts?repo={}",
            repo.path().display()
        );
        let (status, json) = request_json(&app, &attempts_path).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["items"].as_array().unwrap().len(), 1);

        let budget_path = format!(
            "/stewardship/sessions/session-1/budget?repo={}",
            repo.path().display()
        );
        let (status, json) = request_json(&app, &budget_path).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["known_cost_microusd"], 1000);
    }

    #[tokio::test]
    async fn stewardship_attempts_refuses_a_session_from_another_repository() {
        let repo_a = temp_git_repo();
        let repo_b = temp_git_repo();
        let identity_a = FilesystemBacklogDiscovery.resolve(repo_a.path()).unwrap();
        let state = make_state();
        {
            let db = state.db.lock().unwrap();
            familiar_ai_storage::DriverRepository::new(db.conn())
                .open_session("session-a", &identity_a.key, "{}")
                .unwrap();
        }
        let app = make_app(state);
        let path = format!(
            "/stewardship/sessions/session-a/attempts?repo={}",
            repo_b.path().display()
        );
        let (status, _json) = request_json(&app, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn stewardship_gates_lists_stopped_attempt() {
        let repo = temp_git_repo();
        let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
        let state = make_state();
        {
            let db = state.db.lock().unwrap();
            let driver = familiar_ai_storage::DriverRepository::new(db.conn());
            driver
                .open_session("session-1", &identity.key, "{}")
                .unwrap();
            let a = driver
                .record_attempt_started("session-1", "PRD-1", "docs/prds/PRD-1.md", Some("exec-1"))
                .unwrap();
            driver
                .record_attempt_finished(
                    "session-1",
                    a,
                    "retained",
                    Some("scope_broadened"),
                    None,
                    Some(5),
                )
                .unwrap();
        }
        let app = make_app(state);
        let path = format!("/stewardship/gates?repo={}", repo.path().display());
        let (status, json) = request_json(&app, &path).await;
        assert_eq!(status, StatusCode::OK);
        let items = json["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["kind"], "stopped_attempt");
    }
}
