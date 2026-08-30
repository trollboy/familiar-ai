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

async fn stewardship_backlog(
    State(state): State<DashboardState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let identity = match resolve_repo_identity(&params) {
        Ok(identity) => identity,
        Err(response) => return *response,
    };
    let db = state.db.lock().unwrap();
    let status = params.get("status").map(String::as_str);
    let cursor = params.get("cursor").map(String::as_str);
    match familiar_ai_daemon::stewardship::list_backlog(
        &db,
        &identity,
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
  body { font-family: system-ui, sans-serif; max-width: 900px; margin: 2rem auto; padding: 0 1rem; background: #fafafa; color: #222; }
  h1 { border-bottom: 2px solid #333; padding-bottom: 0.5rem; }
  h2 { margin-top: 2rem; color: #555; }
  .card { background: #fff; border: 1px solid #ddd; border-radius: 6px; padding: 1rem; margin: 0.5rem 0; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 0.5rem; }
  .stat { text-align: center; }
  .stat .num { font-size: 2rem; font-weight: bold; color: #2563eb; }
  .stat .label { font-size: 0.85rem; color: #777; }
  table { border-collapse: collapse; width: 100%; }
  th, td { text-align: left; padding: 0.4rem 0.6rem; border-bottom: 1px solid #eee; }
  th { background: #f5f5f5; font-weight: 600; }
  .ok { color: #16a34a; } .err { color: #dc2626; } .warn { color: #d97706; }
  .mono { font-family: monospace; font-size: 0.85rem; }
  #loading { text-align: center; padding: 2rem; color: #999; }
</style>
</head>
<body>
<h1>Familiar</h1>
<div id="loading">Loading…</div>
<div id="content" style="display:none">
  <div id="health-section"></div>
  <h2>Stats</h2>
  <div id="stats-section" class="grid"></div>
  <h2>Projects</h2>
  <div id="projects-section"></div>
  <h2>Recent Activity</h2>
  <div id="recent-section"></div>
</div>
<script>
async function load() {
  try {
    const [health, stats, projects, recent] = await Promise.all([
      fetch('/health').then(r => r.json()),
      fetch('/stats').then(r => r.json()),
      fetch('/projects').then(r => r.json()),
      fetch('/recent').then(r => r.json()),
    ]);
    document.getElementById('loading').style.display = 'none';
    document.getElementById('content').style.display = 'block';
    renderHealth(health);
    renderStats(stats);
    renderProjects(projects.projects || []);
    renderRecent(recent);
  } catch(e) {
    document.getElementById('loading').textContent = 'Failed to load: ' + e;
  }
}
function renderHealth(h) {
  const llm = h.llm || {};
  const el = document.getElementById('health-section');
  el.innerHTML = `<div class="card">
    <strong>Daemon</strong> v${h.version || '?'} (${h.git_sha || '?'}) &middot;
    uptime ${Math.floor((h.daemon_uptime_secs||0)/60)}m &middot;
    DB: <span class="${h.db_reachable?'ok':'err'}">${h.db_reachable?'OK':'unreachable'}</span> &middot;
    LLM: <span class="${llm.healthy?'ok':llm.loaded?'warn':'err'}">${llm.loaded?(llm.healthy?'healthy':'degraded'):'off'}</span>
    ${llm.backend?' ('+llm.backend+')':''}
    ${llm.last_error?'<br><small class="err">'+llm.last_error+'</small>':''}
  </div>`;
}
function renderStats(s) {
  const el = document.getElementById('stats-section');
  const items = [
    ['Projects', s.projects], ['Active', s.active_projects],
    ['Summaries', s.file_summaries], ['Decisions', s.decisions],
    ['Rollups', s.session_rollups],
  ];
  el.innerHTML = items.map(([l,n])=>`<div class="card stat"><div class="num">${n}</div><div class="label">${l}</div></div>`).join('');
}
function renderProjects(ps) {
  const el = document.getElementById('projects-section');
  if(!ps.length) { el.innerHTML='<div class="card">No projects yet.</div>'; return; }
  el.innerHTML = `<table><tr><th>Name</th><th>Root</th><th>Files</th><th>Decisions</th><th>Rollups</th></tr>
    ${ps.map(p=>`<tr><td>${p.name}</td><td class="mono">${p.repo_root}</td><td>${p.file_summaries}</td><td>${p.decisions}</td><td>${p.session_rollups}</td></tr>`).join('')}</table>`;
}
function renderRecent(r) {
  const el = document.getElementById('recent-section');
  let html = '';
  if(r.recent_summaries && r.recent_summaries.length) {
    html += '<h3>File Summaries</h3><table><tr><th>Path</th><th>Summary</th></tr>';
    html += r.recent_summaries.slice(0,5).map(s=>`<tr><td class="mono">${s.path}</td><td>${s.summary.slice(0,120)}</td></tr>`).join('');
    html += '</table>';
  }
  if(r.recent_decisions && r.recent_decisions.length) {
    html += '<h3>Decisions</h3><table><tr><th>Title</th><th>Summary</th></tr>';
    html += r.recent_decisions.slice(0,5).map(d=>`<tr><td>${d.title}</td><td>${d.summary.slice(0,120)}</td></tr>`).join('');
    html += '</table>';
  }
  if(r.recent_rollups && r.recent_rollups.length) {
    html += '<h3>Session Rollups</h3><table><tr><th>Summary</th></tr>';
    html += r.recent_rollups.slice(0,5).map(ro=>`<tr><td>${ro.summary.slice(0,200)}</td></tr>`).join('');
    html += '</table>';
  }
  if(!html) html = '<div class="card">No recent activity.</div>';
  el.innerHTML = html;
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
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        DashboardState {
            db: Arc::new(Mutex::new(db)),
            status: Arc::new(Mutex::new(AppStatus::new())),
            router: Arc::new(InferenceRouter::new(&InferenceConfig::default())),
            start_time: Utc::now(),
        }
    }

    fn make_app(state: DashboardState) -> Router {
        Router::new()
            .route("/", get(html_page))
            .route("/health", get(health))
            .route("/stats", get(stats_endpoint))
            .route("/projects", get(projects))
            .route("/recent", get(recent))
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
