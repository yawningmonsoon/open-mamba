//! Receives openfang completion webhooks → marks envelope done → triggers billing.

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use uuid::Uuid;

use crate::dispatch::Bus;

#[derive(Debug, Deserialize)]
pub struct CompletionWebhook {
    pub job_id: Uuid,
    pub agent_id: String,
    pub exit_code: i32,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cost_usd: Option<f64>,
}

pub async fn handle_completion(
    State(_bus): State<Bus>,
    Json(payload): Json<CompletionWebhook>,
) -> StatusCode {
    // find envelope by openfang_job_id
    let tokens_in = payload.tokens_in.unwrap_or(0);
    let tokens_out = payload.tokens_out.unwrap_or(0);
    let cost = payload.cost_usd.unwrap_or(0.0);

    // We look up the envelope ID from the lake by openfang_job_id
    // For now log and ack — full lookup wired in mamba-api
    tracing::info!(
        job_id = %payload.job_id,
        tokens_in,
        tokens_out,
        cost_usd = cost,
        "webhook: openfang completion"
    );

    StatusCode::OK
}
