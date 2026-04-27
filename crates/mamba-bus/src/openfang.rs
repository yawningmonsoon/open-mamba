//! openfang HTTP client — direct one-shot agent dispatch.
//!
//! We previously routed tasks through openfang's cron scheduler with `at:`
//! schedules, but openfang re-fires `At` jobs on every tick (it's a
//! schedule, not a queue), so each task ran 10x+. The fix is to call the
//! agent's `/message` endpoint directly — a synchronous one-shot turn that
//! returns the response, token counts, and cost.
//!
//! Mamba's `Bus` is the durable queue (DuckDB lake); openfang is just the
//! agent runtime we delegate each turn to.

use anyhow::{anyhow, bail, Context, Result};
use mamba_types::envelope::TaskEnvelope;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
struct MessageRequest<'a> {
    message: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct MessageResponse {
    pub response: String,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// Dispatch outcome — what the worker writes back to the lake.
#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub response: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

pub struct OpenfangClient {
    base_url: String,
    http: reqwest::Client,
}

impl OpenfangClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::builder()
                // Agent turns can take 60-300s for complex tasks (file I/O,
                // multi-step tool use). Generous timeout, since the dispatch
                // is async-spawned anyway.
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .expect("build reqwest client"),
        }
    }

    pub fn from_env() -> Self {
        let url = std::env::var("OPENFANG_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:4200".to_string());
        Self::new(url)
    }

    /// Look up the openfang agent UUID by slug (`name` field on the agent).
    pub async fn resolve_agent_id(&self, slug: &str) -> Result<String> {
        let resp: serde_json::Value = self
            .http
            .get(format!("{}/api/agents", self.base_url))
            .send()
            .await?
            .json()
            .await?;
        let agents = resp.as_array().ok_or_else(|| anyhow!("agents not array"))?;
        for a in agents {
            if a.get("name").and_then(|n| n.as_str()) == Some(slug) {
                if let Some(id) = a.get("id").and_then(|i| i.as_str()) {
                    return Ok(id.to_string());
                }
            }
        }
        bail!("openfang agent '{}' not found", slug)
    }

    /// One-shot agent turn. Returns when the agent has produced its reply
    /// (or errors). Caller is responsible for the durable status update.
    pub async fn dispatch(
        &self,
        envelope: &TaskEnvelope,
        agent_uuid: &str,
    ) -> Result<DispatchResult> {
        // Skill prefix preserved for compatibility with openfang's slash
        // command convention; the Claude CLI doesn't interpret it (verified
        // 2026-04-27 — `/skill` produces "Unknown command" but is harmless).
        let message = match &envelope.skill {
            Some(skill) => format!("/skill {skill}\n\n{}", envelope.payload),
            None => envelope.payload.clone(),
        };

        let url = format!("{}/api/agents/{}/message", self.base_url, agent_uuid);
        let body = MessageRequest { message: &message };

        let resp = self.http.post(&url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("openfang send_message failed {}: {}", status, text);
        }

        let parsed: MessageResponse = resp
            .json()
            .await
            .context("parse openfang MessageResponse")?;

        Ok(DispatchResult {
            response: parsed.response,
            input_tokens: parsed.input_tokens as i64,
            output_tokens: parsed.output_tokens as i64,
            cost_usd: parsed.cost_usd.unwrap_or(0.0),
        })
    }
}
