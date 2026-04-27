/// DDL for all DuckDB tables. Run once on startup (idempotent).
pub const INIT_SQL: &str = r#"
-- ── Task envelopes ────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS task_envelopes (
    id              UUID        PRIMARY KEY,
    project         VARCHAR     NOT NULL,
    source          VARCHAR     NOT NULL,
    assigned_agent  VARCHAR     NOT NULL,
    skill           VARCHAR,
    model           VARCHAR     NOT NULL,
    payload         TEXT        NOT NULL,
    priority        TINYINT     NOT NULL DEFAULT 5,
    status          VARCHAR     NOT NULL DEFAULT 'pending',
    openfang_job_id UUID,
    tokens_in       BIGINT,
    tokens_out      BIGINT,
    cost_usd        DOUBLE,
    encrypted_payload TEXT,
    chain_tx        VARCHAR,
    created_at      TIMESTAMPTZ NOT NULL,
    dispatched_at   TIMESTAMPTZ,
    completed_at    TIMESTAMPTZ,
    response        TEXT
);
-- Idempotent column add for existing DBs.
ALTER TABLE task_envelopes ADD COLUMN IF NOT EXISTS response TEXT;
CREATE INDEX IF NOT EXISTS idx_te_status     ON task_envelopes(status);
CREATE INDEX IF NOT EXISTS idx_te_project    ON task_envelopes(project);
CREATE INDEX IF NOT EXISTS idx_te_agent      ON task_envelopes(assigned_agent);
CREATE INDEX IF NOT EXISTS idx_te_completed  ON task_envelopes(completed_at DESC);

-- ── Billing records ───────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS billing_records (
    id              UUID        PRIMARY KEY,
    task_id         UUID        NOT NULL,
    project         VARCHAR     NOT NULL,
    agent_slug      VARCHAR     NOT NULL,
    model           VARCHAR     NOT NULL,
    tokens_in       BIGINT      NOT NULL DEFAULT 0,
    tokens_out      BIGINT      NOT NULL DEFAULT 0,
    cost_usd        DOUBLE      NOT NULL DEFAULT 0,
    encrypted_log   TEXT        NOT NULL,
    chain_tx        VARCHAR,
    chain_block     UBIGINT,
    created_at      TIMESTAMPTZ NOT NULL
);

-- ── On-chain log ──────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS chain_logs (
    id              UUID        PRIMARY KEY,
    kind            VARCHAR     NOT NULL,
    task_id         UUID,
    payload_hash    VARCHAR     NOT NULL,
    tx_hash         VARCHAR     NOT NULL,
    block_number    UBIGINT,
    chain_id        UBIGINT     NOT NULL,
    submitted_at    TIMESTAMPTZ NOT NULL,
    confirmed_at    TIMESTAMPTZ
);

-- ── Token consumption rollup (Mamba-SSM state snapshots) ─────────────────────
CREATE TABLE IF NOT EXISTS token_snapshots (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    snapshot_at     TIMESTAMPTZ NOT NULL,
    project         VARCHAR,
    agent_slug      VARCHAR,
    model           VARCHAR,
    period          VARCHAR     NOT NULL, -- 'hourly','daily','total'
    tokens_in       BIGINT      NOT NULL DEFAULT 0,
    tokens_out      BIGINT      NOT NULL DEFAULT 0,
    cost_usd        DOUBLE      NOT NULL DEFAULT 0,
    task_count      BIGINT      NOT NULL DEFAULT 0
);

-- ── Agent registry mirror ─────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS agent_registry (
    slug            VARCHAR     PRIMARY KEY,
    role            VARCHAR     NOT NULL,
    model           VARCHAR     NOT NULL,
    base_url        VARCHAR,
    description     TEXT,
    skills          JSON,
    delivery_channel VARCHAR,
    delivery_to     VARCHAR,
    updated_at      TIMESTAMPTZ NOT NULL
);

-- ── Solver outcomes (X1: self-learning loop) ──────────────────────────────────
-- One row per intent the solver evaluated. `decision` is one of:
--   'executed' | 'skipped' | 'failed'
-- `predicted_*` come from the pre-flight estimator; `actual_*` are filled in
-- post-fill from the receipt.
CREATE TABLE IF NOT EXISTS solver_outcomes (
    ts                       TIMESTAMPTZ NOT NULL,
    intent_id                VARCHAR     NOT NULL,
    protocol                 VARCHAR     NOT NULL,
    src_chain                UBIGINT     NOT NULL,
    dst_chain                UBIGINT     NOT NULL,
    decision                 VARCHAR     NOT NULL,
    tx_hash                  VARCHAR,
    predicted_gas            UBIGINT,
    actual_gas               UBIGINT,
    effective_gas_price_wei  VARCHAR,
    predicted_profit_usd     DOUBLE,
    actual_profit_usd        DOUBLE,
    skip_reason              VARCHAR,
    error                    VARCHAR
);
CREATE INDEX IF NOT EXISTS idx_outcomes_intent   ON solver_outcomes(intent_id);
CREATE INDEX IF NOT EXISTS idx_outcomes_ts       ON solver_outcomes(ts DESC);
CREATE INDEX IF NOT EXISTS idx_outcomes_protocol ON solver_outcomes(protocol);
CREATE INDEX IF NOT EXISTS idx_outcomes_decision ON solver_outcomes(decision);

-- ── Solver skip-rules (inferred weekly by nemotron) ───────────────────────────
-- The solver reads `active=true` rows at startup and applies them as filters
-- before invoking the executor. `rule_json` is a free-form JSON predicate; the
-- canonical shape is documented in TAIFOON_SOLVER_DELIVERY_SCOPE.md §X1.
CREATE TABLE IF NOT EXISTS solver_skip_rules (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    protocol        VARCHAR     NOT NULL,
    rule_json       TEXT        NOT NULL,
    description     VARCHAR,
    confidence      DOUBLE      NOT NULL DEFAULT 0.0,
    sample_size     UBIGINT     NOT NULL DEFAULT 0,
    active          BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL,
    superseded_at   TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_skip_rules_active   ON solver_skip_rules(active);
CREATE INDEX IF NOT EXISTS idx_skip_rules_protocol ON solver_skip_rules(protocol);

-- ── Webhook triggers ──────────────────────────────────────────────────────────
-- A registered hook URL (`POST /webhooks/:hook_id`) maps to a task template.
-- When the endpoint is hit, the request body is merged into `payload_template`
-- (treated as a literal string with no substitution today; future versions can
-- add Liquid/Handlebars). The result becomes a new task envelope.
--
-- Hook IDs are short URL-safe strings the user picks. They're public — the
-- secret comes from the Bearer key on the trigger endpoint, gated by the
-- existing MAMBA_API_KEY middleware.
CREATE TABLE IF NOT EXISTS webhooks (
    hook_id          VARCHAR     PRIMARY KEY,
    project          VARCHAR     NOT NULL,
    assigned_agent   VARCHAR     NOT NULL,
    skill            VARCHAR,
    model            VARCHAR     NOT NULL,
    payload_template TEXT        NOT NULL,
    priority         TINYINT     NOT NULL DEFAULT 5,
    enabled          BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at       TIMESTAMPTZ NOT NULL,
    last_fired_at    TIMESTAMPTZ,
    fire_count       UBIGINT     NOT NULL DEFAULT 0
);

-- ── Cron schedules ────────────────────────────────────────────────────────────
-- Each row is a recurring task template. `cron_expr` is a 5-field cron
-- expression in UTC (`min hour dom month dow`). `next_run_at` is recomputed
-- after each fire. `enabled=FALSE` pauses the schedule without deleting it.
CREATE TABLE IF NOT EXISTS schedules (
    id               VARCHAR     PRIMARY KEY,
    cron_expr        VARCHAR     NOT NULL,
    project          VARCHAR     NOT NULL,
    assigned_agent   VARCHAR     NOT NULL,
    skill            VARCHAR,
    model            VARCHAR     NOT NULL,
    payload          TEXT        NOT NULL,
    priority         TINYINT     NOT NULL DEFAULT 5,
    enabled          BOOLEAN     NOT NULL DEFAULT TRUE,
    next_run_at      TIMESTAMPTZ NOT NULL,
    last_fired_at    TIMESTAMPTZ,
    fire_count       UBIGINT     NOT NULL DEFAULT 0,
    created_at       TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_schedules_due ON schedules(enabled, next_run_at);

-- ── Workflows (DAGs of task templates) ────────────────────────────────────────
-- A workflow is an ordered list of steps. Each step is a task template; the
-- output of step N is passed as the `__previous` field in step N+1's payload.
-- For now we only support linear chains — branching is a follow-up.
--
-- `steps_json` is an array of:
--   {
--     "name":           "step-1",
--     "project":        "demo",
--     "assigned_agent": "coder",
--     "skill":          null,
--     "model":          "claude-opus-4-7",
--     "payload":        "string template",
--     "priority":       5
--   }
CREATE TABLE IF NOT EXISTS workflows (
    id           VARCHAR     PRIMARY KEY,
    name         VARCHAR     NOT NULL,
    steps_json   TEXT        NOT NULL,
    enabled      BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at   TIMESTAMPTZ NOT NULL,
    last_run_at  TIMESTAMPTZ,
    run_count    UBIGINT     NOT NULL DEFAULT 0
);

-- ── Workflow runs (one per launch) ────────────────────────────────────────────
-- `outputs_json` is an array of completed step outputs (responses), grown as
-- the run progresses. `current_step` is the next step index to dispatch (0
-- means nothing fired yet, len(steps) means all done).
CREATE TABLE IF NOT EXISTS workflow_runs (
    run_id          VARCHAR     PRIMARY KEY,
    workflow_id     VARCHAR     NOT NULL,
    status          VARCHAR     NOT NULL DEFAULT 'pending',
    current_step    INTEGER     NOT NULL DEFAULT 0,
    outputs_json    TEXT        NOT NULL DEFAULT '[]',
    initial_input   TEXT,
    started_at      TIMESTAMPTZ NOT NULL,
    completed_at    TIMESTAMPTZ,
    error           VARCHAR,
    current_task_id UUID
);
CREATE INDEX IF NOT EXISTS idx_runs_status ON workflow_runs(status);
CREATE INDEX IF NOT EXISTS idx_runs_task   ON workflow_runs(current_task_id);
"#;
