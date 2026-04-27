//! Client for the Taifoon Nemotron inference API.
//!
//! Public endpoint: https://scanner.taifoon.dev/api/intel/<model>/generate
//! Models: taifoon | polymarket | algotrada
//!
//! Response: { response, tokens, duration, tokens_per_second }

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

const PUBLIC_BASE: &str = "https://scanner.taifoon.dev/api/intel";

/// Which adapter to route to on the GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NemotronAdapter {
    Taifoon,
    Polymarket,
    Algotrada,
}

impl NemotronAdapter {
    pub fn slug(&self) -> &'static str {
        match self {
            Self::Taifoon => "taifoon",
            Self::Polymarket => "polymarket",
            Self::Algotrada => "algotrada",
        }
    }

    /// Parse from open-mamba model string e.g. "nemotron/taifoon"
    pub fn from_model_str(s: &str) -> Option<Self> {
        let lower = s.to_lowercase();
        if lower.contains("taifoon") { return Some(Self::Taifoon); }
        if lower.contains("polymarket") { return Some(Self::Polymarket); }
        if lower.contains("algotrada") { return Some(Self::Algotrada); }
        None
    }
}

#[derive(Debug, Serialize)]
struct GenerateRequest<'a> {
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenerateResponse {
    pub response: String,
    pub tokens: u32,
    pub duration: f64,
    pub tokens_per_second: f64,
    pub model: Option<String>,
}

pub struct NemotronClient {
    base_url: String,
    http: reqwest::Client,
}

impl NemotronClient {
    pub fn new() -> Self {
        Self::with_base(PUBLIC_BASE)
    }

    pub fn with_base(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap(),
        }
    }

    pub async fn generate(
        &self,
        adapter: NemotronAdapter,
        prompt: &str,
        system: Option<&str>,
        max_tokens: Option<u32>,
    ) -> Result<GenerateResponse> {
        let url = format!("{}/{}/generate", self.base_url, adapter.slug());
        let body = GenerateRequest {
            prompt,
            max_tokens,
            temperature: Some(0.3),
            system,
        };
        debug!("nemotron {} → {}", adapter.slug(), &prompt[..prompt.len().min(80)]);
        let resp = self.http.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("nemotron {} returned {}: {}", adapter.slug(), status, text);
        }
        Ok(resp.json().await?)
    }

    pub async fn health_all(&self) -> Vec<(NemotronAdapter, bool)> {
        let check = |adapter: NemotronAdapter| {
            let url = format!("{}/{}/health", self.base_url, adapter.slug());
            let http = self.http.clone();
            async move {
                let ok = http.get(&url).send().await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                (adapter, ok)
            }
        };
        let (t, p, a) = tokio::join!(
            check(NemotronAdapter::Taifoon),
            check(NemotronAdapter::Polymarket),
            check(NemotronAdapter::Algotrada),
        );
        vec![t, p, a]
    }
}

impl Default for NemotronClient {
    fn default() -> Self { Self::new() }
}
