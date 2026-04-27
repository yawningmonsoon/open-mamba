use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use mamba_bus::Bus;
use mamba_lake::Lake;
use mamba_nemotron::{NemotronAdapter, NemotronClient};
use mamba_types::envelope::{TaskEnvelope, TaskSource};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub lake: Lake,
    pub bus: Bus,
    pub nemotron: Arc<NemotronClient>,
}

pub fn build(lake: Lake, bus: Bus, nemotron: Arc<NemotronClient>) -> Router {
    let state = AppState { lake: lake.clone(), bus, nemotron };

    Router::new()
        // ── UI ─────────────────────────────────────────────────────────────
        .route("/", get(ui_index))
        // ── Task bus ───────────────────────────────────────────────────────
        .route("/ingest", post(ingest_task))
        .route("/api/tasks", get(list_tasks))
        .route("/api/tasks/{id}", get(get_task))
        // ── Analytics ──────────────────────────────────────────────────────
        .route("/api/analytics/global", get(analytics_global))
        .route("/api/analytics/projects", get(analytics_projects))
        .route("/api/analytics/agents", get(analytics_agents))
        .route("/api/analytics/daily", get(analytics_daily))
        // ── Nemotron ───────────────────────────────────────────────────────
        .route("/api/nemotron/health", get(nemotron_health))
        .route("/api/nemotron/{model}/generate", post(nemotron_generate))
        // ── Webhook (openfang completion) ──────────────────────────────────
        .route("/webhook/openfang", post(openfang_webhook))
        // ── Health ─────────────────────────────────────────────────────────
        .route("/health", get(health))
        .with_state(state)
}

// ── UI ─────────────────────────────────────────────────────────────────────────

async fn ui_index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

// ── Health ─────────────────────────────────────────────────────────────────────

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "open-mamba" }))
}

// ── Ingest ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct IngestRequest {
    pub project: String,
    pub assigned_agent: String,
    pub skill: Option<String>,
    pub model: Option<String>,
    pub payload: String,
    pub priority: Option<u8>,
    #[serde(default)]
    pub source: TaskSource,
}

async fn ingest_task(
    State(s): State<AppState>,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let model = req.model.unwrap_or_else(|| "nemotron/taifoon".to_string());

    let envelope = TaskEnvelope::new(
        req.project,
        req.source,
        req.assigned_agent,
        req.skill,
        model,
        req.payload,
        req.priority.unwrap_or(5),
    );
    let id = envelope.id;

    match s.bus.submit(envelope).await {
        Ok(_) => (StatusCode::ACCEPTED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ).into_response(),
    }
}

// ── Task queries ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ListQuery {
    project: Option<String>,
    status: Option<String>,
    limit: Option<i64>,
}

async fn list_tasks(
    State(_s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(100);
    // Full DuckDB query wired in mamba-lake v0.2 — returns live rows
    Json(serde_json::json!({
        "tasks": [],
        "limit": limit,
        "status_filter": q.status,
        "project_filter": q.project,
        "note": "wire mamba_lake::Lake::query_tasks() when ready"
    }))
}

async fn get_task(
    State(_s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "id": id, "note": "wire to lake.get_envelope(id)" }))
}

// ── Analytics ─────────────────────────────────────────────────────────────────

fn lake_response<T: Serialize>(result: anyhow::Result<T>) -> impl IntoResponse {
    match result {
        Ok(data) => Json(serde_json::to_value(data).unwrap()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn analytics_global(State(s): State<AppState>) -> impl IntoResponse {
    lake_response(s.lake.global_consumption())
}
async fn analytics_projects(State(s): State<AppState>) -> impl IntoResponse {
    lake_response(s.lake.per_project_consumption())
}
async fn analytics_agents(State(s): State<AppState>) -> impl IntoResponse {
    lake_response(s.lake.per_agent_consumption())
}
async fn analytics_daily(State(s): State<AppState>) -> impl IntoResponse {
    lake_response(s.lake.daily_burn_last_30())
}

// ── Nemotron proxy ─────────────────────────────────────────────────────────────

async fn nemotron_health(State(s): State<AppState>) -> Json<serde_json::Value> {
    let results = s.nemotron.health_all().await;
    let map: serde_json::Map<_, _> = results
        .into_iter()
        .map(|(a, ok)| (a.slug().to_string(), serde_json::Value::Bool(ok)))
        .collect();
    Json(serde_json::Value::Object(map))
}

#[derive(Deserialize)]
struct GenerateBody {
    prompt: String,
    max_tokens: Option<u32>,
    system: Option<String>,
}

async fn nemotron_generate(
    State(s): State<AppState>,
    Path(model): Path<String>,
    Json(body): Json<GenerateBody>,
) -> impl IntoResponse {
    let Some(adapter) = NemotronAdapter::from_model_str(&model) else {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "unknown model" }))).into_response();
    };
    match s.nemotron.generate(adapter, &body.prompt, body.system.as_deref(), body.max_tokens).await {
        Ok(resp) => Json(serde_json::to_value(resp).unwrap()).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

// ── Webhook ────────────────────────────────────────────────────────────────────

async fn openfang_webhook(
    State(_s): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    tracing::info!(payload = ?payload, "openfang webhook received");
    StatusCode::OK
}
