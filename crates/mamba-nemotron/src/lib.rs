//! Pluggable inference provider client.
//!
//! The bus dispatches `nemotron/<adapter>` tasks through this crate.
//! Adapters are arbitrary string slugs the upstream provider exposes
//! (e.g. `taifoon`, `polymarket`, `algotrada`) — open-mamba does not
//! ship a default endpoint. You bring your own.
//!
//! ## Configuration
//!
//! - `NEMOTRON_BASE_URL` — required. The provider's base URL. Must expose
//!   `<base>/<adapter>/generate` (POST JSON) and `<base>/<adapter>/health`.
//! - `TAIFOON_GRID_KEY` — optional. Taifoon-grid API key (`taif-…`) sent as
//!   `x-taifoon-key`. Use this when the upstream is a taifoon-grid
//!   endpoint. Get a key by registering a wallet at
//!   `<grid>/api/grid/register` (deterministic, derived on-chain from
//!   wallet + chainId).
//! - `NEMOTRON_API_KEY` — optional. Sent as `Authorization: Bearer …`.
//!   Use for non-taifoon providers (vLLM, Ollama, generic OpenAI-compatible).
//!
//! Both auth headers are forwarded if both are set, so a single client
//! can talk to either kind of provider depending on what it expects.
//!
//! ## Cost model (taifoon-grid path)
//!
//! Each `generate` call deducts grid credits per the provider's posted
//! weights (see `GRID_PRICING_PLANS.md` upstream). The grid contract
//! is the source of truth for balance + API-key validity — open-mamba
//! is purely the dispatcher.
//!
//! Response shape (any provider must conform):
//!   { response: string, tokens: u32, duration: f64, tokens_per_second: f64,
//!     model: optional string }

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Which adapter to route to. Slug is forwarded verbatim in the URL.
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
    base_url: Option<String>,
    /// Bearer token for OpenAI-compatible / generic providers.
    api_key: Option<String>,
    /// Taifoon-grid key (`taif-…`), sent as `x-taifoon-key`. Maps to a
    /// registered wallet's prepaid credit balance on the grid contract.
    grid_key: Option<String>,
    http: reqwest::Client,
}

impl NemotronClient {
    /// Construct from env. Returns a client that will refuse to dispatch if
    /// `NEMOTRON_BASE_URL` is unset — open-mamba ships no default endpoint.
    pub fn from_env() -> Self {
        let base_url = std::env::var("NEMOTRON_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty());
        let api_key = std::env::var("NEMOTRON_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        let grid_key = std::env::var("TAIFOON_GRID_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        Self {
            base_url,
            api_key,
            grid_key,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap(),
        }
    }

    /// Construct with an explicit base URL. Reads keys from env.
    pub fn with_base(base_url: impl Into<String>) -> Self {
        let base = base_url.into();
        let base = if base.is_empty() { None } else { Some(base) };
        Self {
            base_url: base,
            api_key: std::env::var("NEMOTRON_API_KEY").ok().filter(|s| !s.is_empty()),
            grid_key: std::env::var("TAIFOON_GRID_KEY").ok().filter(|s| !s.is_empty()),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap(),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.base_url.is_some()
    }

    fn require_base(&self) -> Result<&str> {
        self.base_url.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "nemotron disabled: set NEMOTRON_BASE_URL to your inference \
                 provider's base URL (and optionally NEMOTRON_API_KEY)"
            )
        })
    }

    pub async fn generate(
        &self,
        adapter: NemotronAdapter,
        prompt: &str,
        system: Option<&str>,
        max_tokens: Option<u32>,
    ) -> Result<GenerateResponse> {
        let base = self.require_base()?;
        let url = format!("{}/{}/generate", base, adapter.slug());
        let body = GenerateRequest {
            prompt,
            max_tokens,
            temperature: Some(0.3),
            system,
        };
        debug!("nemotron {} → {}", adapter.slug(), &prompt[..prompt.len().min(80)]);
        let mut req = self.http.post(&url).json(&body);
        if let Some(ref key) = self.grid_key {
            req = req.header("x-taifoon-key", key);
        }
        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("nemotron {} returned {}: {}", adapter.slug(), status, text);
        }
        Ok(resp.json().await?)
    }

    pub async fn health_all(&self) -> Vec<(NemotronAdapter, bool)> {
        let Some(base) = self.base_url.clone() else {
            return vec![
                (NemotronAdapter::Taifoon, false),
                (NemotronAdapter::Polymarket, false),
                (NemotronAdapter::Algotrada, false),
            ];
        };
        let api_key = self.api_key.clone();
        let grid_key = self.grid_key.clone();
        let check = |adapter: NemotronAdapter| {
            let url = format!("{}/{}/health", base, adapter.slug());
            let http = self.http.clone();
            let api = api_key.clone();
            let grid = grid_key.clone();
            async move {
                let mut req = http.get(&url);
                if let Some(k) = grid {
                    req = req.header("x-taifoon-key", k);
                }
                if let Some(k) = api {
                    req = req.bearer_auth(k);
                }
                let ok = req
                    .send()
                    .await
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
    fn default() -> Self { Self::from_env() }
}
