//! HTTP webhook + cron trigger routes.
//!
//! These are the n8n-equivalent surface area: anyone can register a
//! webhook URL or a cron schedule, and incoming requests / scheduled
//! firings produce task envelopes via the existing `Bus`.
//!
//! Two write paths:
//!
//! - `POST /webhooks/:hook_id`  — anyone with the URL can trigger; rate
//!   limiting / auth is the operator's responsibility (use a non-guessable
//!   `hook_id` and/or front the server with auth gating beyond the
//!   built-in `MAMBA_API_KEY`). The endpoint is intentionally public so
//!   external services (GitHub Actions, Stripe webhooks, etc.) can hit
//!   it without bearer tokens.
//! - `POST /api/schedules`      — register/update a cron schedule. Gated
//!   by `MAMBA_API_KEY` (it's a write that consumes resources later).
//!
//! Read paths (`GET /api/webhooks`, `GET /api/schedules`) are open.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use mamba_lake::{Schedule, Webhook};
use mamba_types::envelope::{TaskEnvelope, TaskSource};
use serde::Deserialize;
use uuid::Uuid;

use crate::routes::AppState;

// ── Webhook CRUD ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct WebhookSpec {
    pub hook_id: String,
    pub project: String,
    pub assigned_agent: String,
    pub skill: Option<String>,
    pub model: Option<String>,
    pub payload_template: String,
    pub priority: Option<u8>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// POST /api/webhooks — register or update a webhook.
pub async fn create_webhook(
    State(s): State<AppState>,
    Json(req): Json<WebhookSpec>,
) -> impl IntoResponse {
    let model = req.model.unwrap_or_else(|| "nemotron/taifoon".to_string());
    let webhook = Webhook {
        hook_id: req.hook_id.clone(),
        project: req.project,
        assigned_agent: req.assigned_agent,
        skill: req.skill,
        model,
        payload_template: req.payload_template,
        priority: req.priority.unwrap_or(5) as i8,
        enabled: req.enabled,
        created_at: Utc::now(),
        last_fired_at: None,
        fire_count: 0,
    };
    tracing::info!(hook_id = %webhook.hook_id, "create_webhook called");
    match s.lake.upsert_webhook(&webhook) {
        Ok(_) => {
            tracing::info!(hook_id = %webhook.hook_id, "webhook upserted");
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "hook_id": req.hook_id })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("upsert_webhook failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /api/webhooks — list registered webhooks.
pub async fn list_webhooks(State(s): State<AppState>) -> impl IntoResponse {
    match s.lake.list_webhooks() {
        Ok(rows) => Json(serde_json::to_value(rows).unwrap()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// DELETE /api/webhooks/:hook_id
pub async fn delete_webhook(
    State(s): State<AppState>,
    Path(hook_id): Path<String>,
) -> impl IntoResponse {
    match s.lake.delete_webhook(&hook_id) {
        Ok(n) if n > 0 => StatusCode::NO_CONTENT.into_response(),
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

/// POST /webhooks/:hook_id — fire the trigger.
///
/// The request body is captured as a JSON value and serialized into the
/// resulting task envelope's payload alongside the configured template.
/// Today both pieces are emitted as a single JSON object:
///
///   { "template": "<webhook.payload_template>", "request": <body> }
///
/// Future versions can run Liquid/Handlebars substitution against
/// `template`. Keeping the contract verbose (vs swapping the template
/// silently) means today's templates can include placeholders that the
/// agent itself parses, which is enough for n8n parity.
pub async fn fire_webhook(
    State(s): State<AppState>,
    Path(hook_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let webhook = match s.lake.get_webhook(&hook_id) {
        Ok(Some(w)) if w.enabled => w,
        Ok(Some(_)) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "webhook is disabled" })),
            )
                .into_response()
        }
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "webhook not found" })),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    };

    let payload = serde_json::json!({
        "template": webhook.payload_template,
        "request":  body,
    });

    let envelope = TaskEnvelope::new(
        webhook.project,
        TaskSource::Webhook,
        webhook.assigned_agent,
        webhook.skill,
        webhook.model,
        payload.to_string(),
        webhook.priority.max(0) as u8,
    );
    let id = envelope.id;

    if let Err(e) = s.bus.submit(envelope).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    let _ = s.lake.note_webhook_fired(&hook_id);

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "task_id": id, "hook_id": hook_id })),
    )
        .into_response()
}

// ── Schedule CRUD ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ScheduleSpec {
    pub id: Option<String>,
    pub cron_expr: String,
    pub project: String,
    pub assigned_agent: String,
    pub skill: Option<String>,
    pub model: Option<String>,
    pub payload: String,
    pub priority: Option<u8>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// POST /api/schedules — register or update a cron schedule.
pub async fn create_schedule(
    State(s): State<AppState>,
    Json(req): Json<ScheduleSpec>,
) -> impl IntoResponse {
    let next_run_at = match crate::cron_util::next_after(&req.cron_expr, Utc::now()) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("invalid cron expression: {e}") })),
            )
                .into_response()
        }
    };

    let id = req.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let model = req.model.unwrap_or_else(|| "nemotron/taifoon".to_string());

    let schedule = Schedule {
        id: id.clone(),
        cron_expr: req.cron_expr,
        project: req.project,
        assigned_agent: req.assigned_agent,
        skill: req.skill,
        model,
        payload: req.payload,
        priority: req.priority.unwrap_or(5) as i8,
        enabled: req.enabled,
        next_run_at,
        last_fired_at: None,
        fire_count: 0,
        created_at: Utc::now(),
    };

    match s.lake.upsert_schedule(&schedule) {
        Ok(_) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": id,
                "next_run_at": schedule.next_run_at.to_rfc3339(),
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/schedules — list schedules.
pub async fn list_schedules(State(s): State<AppState>) -> impl IntoResponse {
    match s.lake.list_schedules() {
        Ok(rows) => Json(serde_json::to_value(rows).unwrap()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// DELETE /api/schedules/:id
pub async fn delete_schedule(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match s.lake.delete_schedule(&id) {
        Ok(n) if n > 0 => StatusCode::NO_CONTENT.into_response(),
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
