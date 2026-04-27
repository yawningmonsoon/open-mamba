//! Main bus dispatcher. Routes TaskEnvelopes to:
//!   - nemotron/taifoon|polymarket|algotrada → mamba-nemotron client (direct)
//!   - all other models → openfang CronJob
//! Also triggers billing_worker on completion.

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
        let nemotron_base = std::env::var("NEMOTRON_BASE_URL")
            .unwrap_or_else(|_| "https://scanner.taifoon.dev/api/intel".to_string());
        Self {
            lake,
            openfang: Arc::new(OpenfangClient::from_env()),
            nemotron: Arc::new(NemotronClient::with_base(nemotron_base)),
            billing: Arc::new(BillingWorker::from_env()),
        }
    }

    /// Accept a new TaskEnvelope, persist it, then dispatch asynchronously.
    pub async fn submit(&self, mut envelope: TaskEnvelope) -> Result<()> {
        self.lake.insert_envelope(&envelope)?;
        info!(id = %envelope.id, project = %envelope.project, agent = %envelope.assigned_agent, "bus: submitted");

        // Dispatch in background — don't block the HTTP response
        let bus = self.clone();
        let env_id = envelope.id;
        tokio::spawn(async move {
            if let Err(e) = bus.route(envelope).await {
                error!(id = %env_id, "bus dispatch failed: {e}");
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

    async fn dispatch_nemotron(&self, envelope: TaskEnvelope, adapter: NemotronAdapter) -> Result<()> {
        info!(id = %envelope.id, adapter = %adapter.slug(), "nemotron dispatch");
        self.lake.update_status(envelope.id, TaskStatus::Running, None)?;

        let system = Some("You are a Taifoon autonomous agent. Be concise and precise.");
        let resp = self.nemotron.generate(adapter, &envelope.payload, system, Some(512)).await?;

        info!(id = %envelope.id, tokens = resp.tokens, "nemotron done");

        self.lake.complete_envelope(
            envelope.id,
            0,
            resp.tokens as i64,
            estimate_cost_usd(0, resp.tokens as i64, adapter.slug()),
            None,
        )?;

        self.billing.process(
            &envelope,
            0,
            resp.tokens as i64,
            estimate_cost_usd(0, resp.tokens as i64, adapter.slug()),
        ).await?;

        Ok(())
    }

    async fn dispatch_openfang(&self, envelope: TaskEnvelope) -> Result<()> {
        info!(id = %envelope.id, agent = %envelope.assigned_agent, "openfang dispatch");
        let agent_uuid = self.openfang.resolve_agent_id(&envelope.assigned_agent).await?;
        let job_id = self.openfang.dispatch(&envelope, &agent_uuid).await?;
        self.lake.update_status(envelope.id, TaskStatus::Dispatched, Some(job_id))?;
        info!(id = %envelope.id, job_id = %job_id, "dispatched to openfang");
        Ok(())
    }
}

/// Rough cost estimate. Nemotron is on-prem GPU so cost = 0 for tokens.
/// Cloud models use standard pricing.
fn estimate_cost_usd(tokens_in: i64, tokens_out: i64, model: &str) -> f64 {
    if model.starts_with("nemotron") || model.starts_with("taifoon") || model.starts_with("polymarket") || model.starts_with("algotrada") {
        return 0.0; // on-prem GPU — $0
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
