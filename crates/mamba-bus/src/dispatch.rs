//! Main bus dispatcher. Routes `TaskEnvelopes` to:
//!
//! - nemotron/taifoon|polymarket|algotrada → mamba-nemotron client (direct)
//! - all other models → openfang one-shot agent turn
//!
//! Also triggers `billing_worker` on completion.

use crate::{billing_worker::BillingWorker, openfang::OpenfangClient};
use anyhow::Result;
use mamba_lake::Lake;
use mamba_nemotron::{NemotronAdapter, NemotronClient};
use mamba_types::envelope::{TaskEnvelope, TaskStatus};
use std::sync::Arc;
use tracing::{error, info};

#[derive(Clone)]
pub struct Bus {
    lake: Lake,
    openfang: Arc<OpenfangClient>,
    nemotron: Arc<NemotronClient>,
    billing: Arc<BillingWorker>,
}

impl Bus {
    pub fn new(lake: Lake) -> Self {
        Self {
            lake,
            openfang: Arc::new(OpenfangClient::from_env()),
            // Reads NEMOTRON_BASE_URL + NEMOTRON_API_KEY from env. No
            // default upstream endpoint — operators bring their own.
            nemotron: Arc::new(NemotronClient::from_env()),
            billing: Arc::new(BillingWorker::from_env()),
        }
    }

    /// Persist a new envelope to the lake. The queue worker (mamba-api's
     /// background task) picks it up on its next poll and dispatches.
     ///
     /// Note: we no longer spawn the dispatch inline — that path lost
     /// failures on auth errors and competed with the worker. A single
     /// place owning the dispatch (the worker) is simpler and more reliable.
    pub async fn submit(&self, envelope: TaskEnvelope) -> Result<()> {
        self.lake.insert_envelope(&envelope)?;
        info!(
            id = %envelope.id,
            project = %envelope.project,
            agent = %envelope.assigned_agent,
            "bus: submitted (worker will dispatch)"
        );
        Ok(())
    }

    /// Re-route an envelope that's already in the lake. Skips the insert
    /// (so we don't get a primary-key conflict) and re-runs the dispatch
    /// path. Used by retry endpoints to unstick tasks that failed at
    /// dispatch time (e.g. openfang was down or the agent slug didn't
    /// exist yet).
    pub async fn redispatch(&self, envelope: TaskEnvelope) -> Result<()> {
        info!(id = %envelope.id, agent = %envelope.assigned_agent, "bus: redispatching");
        let bus = self.clone();
        let env_id = envelope.id;
        tokio::spawn(async move {
            if let Err(e) = bus.route(envelope).await {
                error!(id = %env_id, "bus redispatch failed: {e}");
            }
        });
        Ok(())
    }

    async fn route(&self, envelope: TaskEnvelope) -> Result<()> {
        if let Some(adapter) = NemotronAdapter::from_model_str(&envelope.model) {
            self.dispatch_nemotron(envelope, adapter).await
        } else {
            self.dispatch_openfang(envelope).await
        }
    }

    /// Blocking dispatch — waits for the agent turn to finish before
    /// returning. Used by the queue worker so the in-flight slot stays
    /// reserved for the full duration of the turn.
    pub async fn route_blocking(&self, envelope: TaskEnvelope) -> Result<()> {
        self.route(envelope).await
    }

    async fn dispatch_nemotron(&self, envelope: TaskEnvelope, adapter: NemotronAdapter) -> Result<()> {
        info!(id = %envelope.id, adapter = %adapter.slug(), "nemotron dispatch");
        self.lake.update_status(envelope.id, TaskStatus::Running, None)?;

        let system = Some("You are a Taifoon autonomous agent. Be concise and precise.");
        let resp = self.nemotron.generate(adapter, &envelope.payload, system, Some(512)).await?;
        let tokens_out = resp.tokens as i64;
        let cost_usd = estimate_cost_usd(0, tokens_out, adapter.slug());

        info!(id = %envelope.id, tokens = resp.tokens, "nemotron done");

        self.lake.complete_envelope(envelope.id, 0, tokens_out, cost_usd, None)?;
        self.billing.process(&envelope, 0, tokens_out, cost_usd).await?;

        Ok(())
    }

    async fn dispatch_openfang(&self, envelope: TaskEnvelope) -> Result<()> {
        info!(id = %envelope.id, agent = %envelope.assigned_agent, "openfang dispatch");
        let agent_uuid = self.openfang.resolve_agent_id(&envelope.assigned_agent).await?;

        // Mark Running before the call so the worker won't pick this row up
        // again while the agent is mid-turn (turns can take minutes).
        self.lake.update_status(envelope.id, TaskStatus::Running, None)?;

        match self.openfang.dispatch(&envelope, &agent_uuid).await {
            Ok(result) => {
                info!(
                    id = %envelope.id,
                    in_tokens = result.input_tokens,
                    out_tokens = result.output_tokens,
                    cost_usd = result.cost_usd,
                    "openfang dispatch ok"
                );
                // Persist the response text first so workflow advancers
                // can see it. Best-effort: a failure here doesn't block
                // marking the task complete.
                let _ = self.lake.save_response(envelope.id, &result.response);
                self.lake.complete_envelope(
                    envelope.id,
                    result.input_tokens,
                    result.output_tokens,
                    result.cost_usd,
                    None,
                )?;
                self.billing
                    .process(&envelope, result.input_tokens, result.output_tokens, result.cost_usd)
                    .await
                    .ok();
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                let is_transient = is_transient_error(&msg);
                let rescheduled = self
                    .lake
                    .record_dispatch_failure(
                        envelope.id,
                        is_transient,
                        /* max_retries */ 2,
                        /* backoff_secs */ if is_transient { 30 } else { 0 },
                    )
                    .unwrap_or(false);
                if rescheduled {
                    info!(
                        id = %envelope.id,
                        "openfang dispatch transient failure, rescheduled (will retry in 30s): {msg}"
                    );
                } else {
                    error!(id = %envelope.id, "openfang dispatch failed permanently: {msg}");
                }
                Err(e)
            }
        }
    }
}

/// Classify a dispatch error message as transient (retryable) vs permanent.
///
/// Transient: network blips, request timeouts, openfang briefly unreachable,
/// Anthropic / nemotron 5xx. These should retry with backoff.
///
/// Permanent: `agent not found`, 4xx from openfang/nemotron, parse errors,
/// malformed envelope. Retrying these wastes tokens and time.
fn is_transient_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    let transient_signals = [
        "timeout",
        "timed out",
        "error sending request",
        "connection refused",
        "connection reset",
        "broken pipe",
        "502 bad gateway",
        "503 service unavailable",
        "504 gateway timeout",
        "tcp connect error",
        "deadline has elapsed",
    ];
    let permanent_signals = [
        "agent ",         // 'openfang agent X not found'
        "missing field",
        "unauthorized",
        "401 ",
        "403 ",
        "invalid api key",
        "license rejected",
    ];
    if permanent_signals.iter().any(|s| m.contains(s)) {
        return false;
    }
    transient_signals.iter().any(|s| m.contains(s))
}

#[cfg(test)]
mod transient_tests {
    use super::is_transient_error;

    #[test]
    fn timeout_is_transient() {
        assert!(is_transient_error("error sending request for url (http://...)"));
        assert!(is_transient_error("operation timed out after 60s"));
        assert!(is_transient_error("nemotron returned 502 Bad Gateway"));
    }

    #[test]
    fn auth_is_permanent() {
        assert!(!is_transient_error("openfang agent 'coder' not found"));
        assert!(!is_transient_error("401 unauthorized"));
        assert!(!is_transient_error("invalid api key"));
    }

    #[test]
    fn unknown_defaults_to_permanent() {
        // Conservative: unknown errors don't waste a retry budget.
        assert!(!is_transient_error("some weird never-before-seen error"));
    }
}

fn estimate_cost_usd(tokens_in: i64, tokens_out: i64, model: &str) -> f64 {
    if NemotronAdapter::from_model_str(model).is_some() {
        return 0.0;
    }
    // claude-sonnet-4-6: $3/M in, $15/M out
    let (in_rate, out_rate) = if model.contains("sonnet") {
        (3.0 / 1_000_000.0, 15.0 / 1_000_000.0)
    } else if model.contains("opus") {
        (15.0 / 1_000_000.0, 75.0 / 1_000_000.0)
    } else if model.contains("haiku") {
        (0.8 / 1_000_000.0, 4.0 / 1_000_000.0)
    } else {
        (3.0 / 1_000_000.0, 15.0 / 1_000_000.0)
    };
    tokens_in as f64 * in_rate + tokens_out as f64 * out_rate
}
