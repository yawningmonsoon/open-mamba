//! HTTP routes for workflow CRUD + run launch.
//!
//! All routes are JSON. Workflows are persisted in DuckDB; runs are
//! also persisted so a server restart resumes them via the workflow
//! advancer poll.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use mamba_lake::{Workflow, WorkflowRun, WorkflowStep};
use serde::Deserialize;
use uuid::Uuid;

use crate::routes::AppState;
use crate::workflow_runner::build_step_envelope;

#[derive(Debug, Deserialize)]
pub struct WorkflowSpec {
    pub id: Option<String>,
    pub name: String,
    pub steps: Vec<WorkflowStep>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct RunSpec {
    /// Optional initial input that gets stored on the run and is
    /// available to the first step as `__workflow.initial_input`.
    #[serde(default)]
    pub initial_input: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RunListQuery {
    pub workflow_id: Option<String>,
    pub limit: Option<i64>,
}

// ── Workflows CRUD ────────────────────────────────────────────────────────────

pub async fn create_workflow(
    State(s): State<AppState>,
    Json(req): Json<WorkflowSpec>,
) -> impl IntoResponse {
    if req.steps.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "workflow needs at least 1 step" })),
        )
            .into_response();
    }

    let id = req.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let workflow = Workflow {
        id: id.clone(),
        name: req.name,
        steps: req.steps,
        enabled: req.enabled,
        created_at: Utc::now(),
        last_run_at: None,
        run_count: 0,
    };
    match s.lake.upsert_workflow(&workflow) {
        Ok(_) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "id": id })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn list_workflows(State(s): State<AppState>) -> impl IntoResponse {
    match s.lake.list_workflows() {
        Ok(rows) => Json(serde_json::to_value(rows).unwrap()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn get_workflow(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match s.lake.get_workflow(&id) {
        Ok(Some(w)) => Json(serde_json::to_value(w).unwrap()).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "workflow not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn delete_workflow(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match s.lake.delete_workflow(&id) {
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

// ── Run launch + inspection ───────────────────────────────────────────────────

/// POST /api/workflows/:id/run — launch a new workflow run.
pub async fn run_workflow(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RunSpec>,
) -> impl IntoResponse {
    let workflow = match s.lake.get_workflow(&id) {
        Ok(Some(w)) if w.enabled => w,
        Ok(Some(_)) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "workflow is disabled" })),
            )
                .into_response()
        }
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "workflow not found" })),
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

    let run_id = Uuid::new_v4().to_string();
    // Step 0 is dispatched right now.
    let step = &workflow.steps[0];
    let mut run = WorkflowRun {
        run_id: run_id.clone(),
        workflow_id: workflow.id.clone(),
        status: "running".into(),
        current_step: 0,
        outputs: Vec::new(),
        initial_input: req.initial_input,
        started_at: Utc::now(),
        completed_at: None,
        error: None,
        current_task_id: None,
    };

    let envelope = match build_step_envelope(step, &run) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    };
    let task_id = envelope.id;

    if let Err(e) = s.bus.submit(envelope).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("bus submit failed: {e}") })),
        )
            .into_response();
    }

    run.current_task_id = Some(task_id);
    if let Err(e) = s.lake.insert_run(&run) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    let _ = s.lake.note_workflow_run_started(&workflow.id);

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "run_id":  run_id,
            "task_id": task_id,
            "step":    0,
        })),
    )
        .into_response()
}

pub async fn get_run(
    State(s): State<AppState>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    match s.lake.get_run(&run_id) {
        Ok(Some(r)) => Json(serde_json::to_value(r).unwrap()).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "run not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn list_runs(
    State(s): State<AppState>,
    Query(q): Query<RunListQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(50);
    match s.lake.list_runs(q.workflow_id.as_deref(), limit) {
        Ok(rows) => Json(serde_json::to_value(rows).unwrap()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
