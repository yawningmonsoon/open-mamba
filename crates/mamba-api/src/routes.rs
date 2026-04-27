use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use axum::http::Request;
use mamba_bus::Bus;
use mamba_lake::{Lake, SolverOutcomeRow, SolverSkipRuleRow};
use mamba_nemotron::{NemotronAdapter, NemotronClient};
use chrono::Utc;
use mamba_types::envelope::{TaskEnvelope, TaskSource, TaskStatus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// Bearer token check for write/expensive endpoints.
///
/// Reads `MAMBA_API_KEY` from the environment at request time. When unset
/// (the default), the middleware is a no-op — useful for local dev. When
/// set, every request must carry `authorization: Bearer <key>` or
/// `x-mamba-key: <key>` and the value must match.
///
/// Designed for the public-deploy scenario: anyone running the bus on the
/// open internet should set MAMBA_API_KEY so random callers can't burn
/// their nemotron / claude budget.
async fn require_api_key(
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let expected = match std::env::var("MAMBA_API_KEY") {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(next.run(req).await), // unset = open mode
    };

    let provided = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .or_else(|| req.headers().get("x-mamba-key").and_then(|h| h.to_str().ok()));

    match provided {
        Some(p) if p == expected => Ok(next.run(req).await),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "missing or invalid api key",
                "hint":  "set MAMBA_API_KEY in env, send `authorization: Bearer <key>` or `x-mamba-key: <key>`"
            })),
        )),
    }
}

#[derive(Clone)]
pub struct AppState {
    pub lake: Lake,
    pub bus: Bus,
    pub nemotron: Arc<NemotronClient>,
}

pub fn build(lake: Lake, bus: Bus, nemotron: Arc<NemotronClient>) -> Router {
    let state = AppState { lake: lake.clone(), bus, nemotron };

    // Protected: spends money / mutates queue. Gated by MAMBA_API_KEY when set.
    let protected = Router::new()
        .route("/ingest", post(ingest_task))
        .route("/api/tasks/:id/retry", post(retry_task))
        .route("/api/tasks/retry-pending", post(retry_pending))
        .route("/api/nemotron/:model/generate", post(nemotron_generate))
        // Publishing skip-rules supersedes the existing rule set — gate it.
        .route("/api/solver/skip-rules", post(publish_skip_rules))
        .route_layer(middleware::from_fn(require_api_key));

    // Public: read-only / health / dashboard. Anyone can call.
    let public = Router::new()
        // ── UI ─────────────────────────────────────────────────────────────
        .route("/", get(ui_index))
        // ── Task queries ───────────────────────────────────────────────────
        .route("/api/tasks", get(list_tasks))
        .route("/api/tasks/:id", get(get_task))
        // ── Analytics ──────────────────────────────────────────────────────
        .route("/api/analytics/global", get(analytics_global))
        .route("/api/analytics/projects", get(analytics_projects))
        .route("/api/analytics/agents", get(analytics_agents))
        .route("/api/analytics/daily", get(analytics_daily))
        // ── Nemotron health (no spend) ─────────────────────────────────────
        .route("/api/nemotron/health", get(nemotron_health))
        // ── Solver self-learning loop ──────────────────────────────────────
        // Solver POSTs outcomes from inside the trusted network; reads of
        // outcomes + active skip-rules are open so the dashboard can show them.
        .route("/api/solver/outcomes", post(ingest_outcome).get(list_outcomes))
        .route("/api/solver/outcomes/count", get(count_outcomes_route))
        .route("/api/solver/skip-rules", get(list_skip_rules))
        // ── Webhook (openfang signs callbacks separately) ──────────────────
        .route("/webhook/openfang", post(openfang_webhook))
        // ── Health ─────────────────────────────────────────────────────────
        .route("/health", get(health));

    public.merge(protected).with_state(state)
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
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(100);
    lake_response(
        s.lake.query_tasks(q.project.as_deref(), q.status.as_deref(), limit)
    )
}

async fn get_task(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match s.lake.get_task(id) {
        Ok(Some(row)) => Json(serde_json::to_value(row).unwrap()).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "not found" }))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

// ── Retry / re-dispatch ───────────────────────────────────────────────────────

/// POST /api/tasks/:id/retry — reset a task to pending so the worker
/// picks it up on its next poll.
async fn retry_task(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match s.lake.reset_to_pending(&[id]) {
        Ok(n) if n > 0 => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "id": id, "reset": true })),
        )
            .into_response(),
        Ok(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/tasks/retry-pending — reset all dispatched/running/failed
/// rows back to pending. The worker drains them on its next poll.
async fn retry_pending(State(s): State<AppState>) -> impl IntoResponse {
    match s.lake.reset_stuck_tasks() {
        Ok(n) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "reset": n })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) fn envelope_from_row(row: &mamba_lake::TaskRow) -> anyhow::Result<TaskEnvelope> {
    let id = Uuid::parse_str(&row.id)?;
    let source = match row.source.as_str() {
        "claude_desktop" => TaskSource::ClaudeDesktop,
        "claude_opus"    => TaskSource::ClaudeOpus,
        "cron"           => TaskSource::Cron,
        "webhook"        => TaskSource::Webhook,
        _                => TaskSource::Api,
    };
    let created_at = row.created_at.parse().unwrap_or_else(|_| Utc::now());
    Ok(TaskEnvelope {
        id,
        project:           row.project.clone(),
        source,
        assigned_agent:    row.assigned_agent.clone(),
        skill:             row.skill.clone(),
        model:             row.model.clone(),
        payload:           row.payload.clone(),
        priority:          row.priority.max(0) as u8,
        status:            TaskStatus::Pending,
        openfang_job_id:   None,
        tokens_in:         row.tokens_in,
        tokens_out:        row.tokens_out,
        cost_usd:          row.cost_usd,
        encrypted_payload: None,
        chain_tx:          None,
        created_at,
        dispatched_at:     None,
        completed_at:      None,
    })
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

// ── Solver self-learning loop (X1) ────────────────────────────────────────────

/// Wire shape posted by the solver after every executed/skipped intent.
/// Matches `taifoon-solver::executor::outcome_log::OutcomeRecord` plus the
/// X1 fields (`predicted_*`, `skip_reason`).
#[derive(Deserialize)]
struct OutcomeIngest {
    #[serde(default = "default_now")]
    ts: chrono::DateTime<Utc>,
    intent_id: String,
    protocol: String,
    src_chain: u64,
    dst_chain: u64,
    decision: String,
    tx_hash: Option<String>,
    /// Pre-flight estimate from gas estimator.
    predicted_gas: Option<u64>,
    /// Receipt-derived `gas_used` (post-fill).
    #[serde(alias = "gas_used")]
    actual_gas: Option<u64>,
    effective_gas_price_wei: Option<String>,
    predicted_profit_usd: Option<f64>,
    actual_profit_usd: Option<f64>,
    skip_reason: Option<String>,
    error: Option<String>,
}

fn default_now() -> chrono::DateTime<Utc> { Utc::now() }

async fn ingest_outcome(
    State(s): State<AppState>,
    Json(req): Json<OutcomeIngest>,
) -> impl IntoResponse {
    let row = SolverOutcomeRow {
        ts: req.ts.to_rfc3339(),
        intent_id: req.intent_id,
        protocol: req.protocol,
        src_chain: req.src_chain,
        dst_chain: req.dst_chain,
        decision: req.decision,
        tx_hash: req.tx_hash,
        predicted_gas: req.predicted_gas,
        actual_gas: req.actual_gas,
        effective_gas_price_wei: req.effective_gas_price_wei,
        predicted_profit_usd: req.predicted_profit_usd,
        actual_profit_usd: req.actual_profit_usd,
        skip_reason: req.skip_reason,
        error: req.error,
    };
    match s.lake.insert_outcome(&row) {
        Ok(_) => (StatusCode::ACCEPTED, Json(serde_json::json!({ "ok": true }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ).into_response(),
    }
}

#[derive(Deserialize)]
struct OutcomeListQuery {
    limit: Option<i64>,
}

async fn list_outcomes(
    State(s): State<AppState>,
    Query(q): Query<OutcomeListQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(100).clamp(1, 5000);
    lake_response(s.lake.recent_outcomes(limit))
}

async fn count_outcomes_route(State(s): State<AppState>) -> impl IntoResponse {
    match s.lake.count_outcomes() {
        Ok(n) => Json(serde_json::json!({ "count": n })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ).into_response(),
    }
}

async fn list_skip_rules(State(s): State<AppState>) -> impl IntoResponse {
    lake_response(s.lake.list_active_skip_rules())
}

#[derive(Deserialize)]
struct PublishSkipRules {
    protocol: String,
    rules: Vec<SkipRuleInput>,
}

#[derive(Deserialize)]
struct SkipRuleInput {
    rule_json: String,
    description: Option<String>,
    confidence: f64,
    sample_size: u64,
}

async fn publish_skip_rules(
    State(s): State<AppState>,
    Json(req): Json<PublishSkipRules>,
) -> impl IntoResponse {
    let now = Utc::now().to_rfc3339();
    let rows: Vec<SolverSkipRuleRow> = req
        .rules
        .into_iter()
        .map(|r| SolverSkipRuleRow {
            id: Uuid::new_v4().to_string(),
            protocol: req.protocol.clone(),
            rule_json: r.rule_json,
            description: r.description,
            confidence: r.confidence,
            sample_size: r.sample_size,
            active: true,
            created_at: now.clone(),
            superseded_at: None,
        })
        .collect();
    match s.lake.replace_skip_rules(&req.protocol, &rows) {
        Ok(n) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "protocol": req.protocol, "published": n })),
        ).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ).into_response(),
    }
}
