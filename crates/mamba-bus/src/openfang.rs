//! openfang REST client — creates CronJob tasks and polls agent status.

use anyhow::{bail, Result};
use mamba_types::envelope::TaskEnvelope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize)]
struct CreateCronJob {
    agent_id: String,
    name: String,
    schedule: CronSchedule,
    action: CronAction,
    delivery: CronDelivery,
    enabled: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CronSchedule {
    At { at: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CronAction {
    AgentTurn {
        message: String,
        model_override: Option<String>,
        timeout_secs: Option<u64>,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CronDelivery {
    Channel { channel: String, to: String },
    None,
}

#[derive(Debug, Deserialize)]
pub struct CronJobCreated {
    pub id: Uuid,
}

pub struct OpenfangClient {
    base_url: String,
    http: reqwest::Client,
    telegram_chat: Option<String>,
}

impl OpenfangClient {
    pub fn new(base_url: impl Into<String>, telegram_chat: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap(),
            telegram_chat,
        }
    }

    pub fn from_env() -> Self {
        let url = std::env::var("OPENFANG_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:4200".to_string());
        let chat = std::env::var("MAMBA_TELEGRAM_CHAT").ok();
        Self::new(url, chat)
    }

    /// Look up the openfang agent UUID by slug name.
    pub async fn resolve_agent_id(&self, slug: &str) -> Result<String> {
        let resp: serde_json::Value = self.http
            .get(format!("{}/api/agents", self.base_url))
            .send()
            .await?
            .json()
            .await?;
        let agents = resp.as_array().ok_or_else(|| anyhow::anyhow!("agents not array"))?;
        for a in agents {
            if a["name"].as_str().unwrap_or("") == slug {
                if let Some(id) = a["id"].as_str() {
                    return Ok(id.to_string());
                }
            }
        }
        bail!("openfang agent '{}' not found", slug)
    }

    pub async fn dispatch(&self, envelope: &TaskEnvelope, agent_uuid: &str) -> Result<Uuid> {
        let delivery = match &self.telegram_chat {
            Some(chat_id) => CronDelivery::Channel {
                channel: "telegram".to_string(),
                to: chat_id.clone(),
            },
            None => CronDelivery::None,
        };

        let message = match &envelope.skill {
            Some(skill) => format!("/skill {skill}\n\n{}", envelope.payload),
            None => envelope.payload.clone(),
        };

        let model_override = if envelope.model.starts_with("nemotron/") {
            None // nemotron handled by mamba-nemotron directly, not via openfang
        } else {
            Some(envelope.model.clone())
        };

        let body = CreateCronJob {
            agent_id: agent_uuid.to_string(),
            name: format!("mamba-{}", envelope.id),
            schedule: CronSchedule::At {
                at: chrono::Utc::now().to_rfc3339(),
            },
            action: CronAction::AgentTurn {
                message,
                model_override,
                timeout_secs: Some(300),
            },
            delivery,
            enabled: true,
        };

        let resp = self.http
            .post(format!("{}/api/cron/jobs", self.base_url))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("openfang cron create failed {}: {}", status, text);
        }

        let created: CronJobCreated = resp.json().await?;
        Ok(created.id)
    }
}
