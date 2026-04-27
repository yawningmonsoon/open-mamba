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
    completed_at    TIMESTAMPTZ
);
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
"#;
