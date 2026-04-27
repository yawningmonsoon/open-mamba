use serde::{Deserialize, Serialize};

/// Standardized agent roles — maps to openfang agent slugs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum AgentRole {
    /// Ships code, fixes bugs. Model: nemotron/taifoon (cheap tasks) or claude-sonnet
    CodeDeliverer,
    /// K8s deploys, docker builds. Model: nemotron/taifoon
    Deployer,
    /// Code review. Model: claude-sonnet
    Auditor,
    /// Cross-project planning. Model: claude-opus
    Planner,
    /// Taifoon protocol intel queries. Model: nemotron/taifoon (native)
    ProtocolIntel,
    /// Polymarket predictions. Model: nemotron/polymarket (native)
    MarketIntel,
    /// Algotrada cross-DEX. Model: nemotron/algotrada (native)
    AlgoTrader,
}

impl AgentRole {
    /// Default model for this role.
    pub fn default_model(&self) -> &'static str {
        match self {
            AgentRole::CodeDeliverer => "nemotron/taifoon",
            AgentRole::Deployer => "nemotron/taifoon",
            AgentRole::Auditor => "claude-sonnet-4-6",
            AgentRole::Planner => "claude-opus-4-7",
            AgentRole::ProtocolIntel => "nemotron/taifoon",
            AgentRole::MarketIntel => "nemotron/polymarket",
            AgentRole::AlgoTrader => "nemotron/algotrada",
        }
    }

    /// Corresponding openfang agent slug.
    pub fn openfang_agent(&self) -> &'static str {
        match self {
            AgentRole::CodeDeliverer => "coder",
            AgentRole::Deployer => "devops-lead",
            AgentRole::Auditor => "code-reviewer",
            AgentRole::Planner => "planner",
            AgentRole::ProtocolIntel => "taifoon-intel",
            AgentRole::MarketIntel => "market-intel",
            AgentRole::AlgoTrader => "algo-trader",
        }
    }
}

/// Runtime agent definition loaded from config/agents/*.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDef {
    pub slug: String,
    pub role: AgentRole,
    pub description: String,
    pub model: String,
    pub base_url: Option<String>,
    pub skills: Vec<String>,
    pub delivery_channel: Option<String>,
    pub delivery_to: Option<String>,
}
