# mamba — control script for open-mamba + openfang

Single command (`mamba up`) brings up the autonomous task delivery pipeline:

```
                  ┌────────────────┐
   POST /ingest → │   open-mamba   │ → routes → ┌────────────┐
                  │   :1337        │            │  openfang  │ → claude-opus-4-7
                  │   DuckDB lake  │            │  :4200     │ → claude-sonnet-4-6
                  │   bus + agents │            │  cron jobs │
                  └────────────────┘            └────────────┘
                          │                            │
                          ▼                            │
                    nemotron/* ────────────────────────┘
                    (scanner.taifoon.dev)
```

## Quick start

```bash
mamba up                 # auth + start everything + smoke test
mamba status             # running state, task counts, agent registry
mamba logs               # tail open-mamba + openfang logs
mamba down               # stop both daemons (alias: stop)
mamba retry-pending      # re-dispatch all pending tasks
mamba retry <task-id>    # re-dispatch a single task
mamba register-agents    # spawn missing agents into openfang
mamba auth               # refresh OAuth token from keychain
```

## How AUTH works (the simple version)

**You don't manage Anthropic credentials.** Tasks tagged with a Claude
model (`claude-opus-4-7`, `claude-sonnet-4-6`, etc.) are routed through
openfang's **`claude-code` provider**, which shells out to the `claude`
CLI you already have on your machine. Whatever auth your `claude` command
has, mamba inherits.

```
mamba    →    openfang    →    claude -p '...' --model opus    →    Anthropic
```

If `claude /login` works in your terminal, mamba works. That's it.

**Mapping** (in `mamba-bus/src/openfang.rs`): any `claude-*opus*` model
becomes `claude-code/opus`; `*sonnet*` → `claude-code/sonnet`;
`*haiku*` → `claude-code/haiku`. Override by sending `claude-code/...`
directly in the `model` field if you want explicit control.

**Why this is preferable to API keys / OAuth refresh**: Claude Code already
manages session refresh, rate limiting, and works with Pro/Max/Team plans
without per-call billing. Going through the CLI inherits all of that.

### Legacy: API-key path (still works)

If you'd rather use a real API key, set `ANTHROPIC_API_KEY=sk-ant-api03-...`
in `~/.mamba/runtime.env`. The bus's model mapper has an "exact prefix"
escape hatch: any model starting with `claude-code/` is left alone, any
model starting with `nemotron/` goes to nemotron, and other strings
fall through verbatim — so set `model: "claude-sonnet-4-20250514"` in
your task to bypass the claude-code routing and use the standard
Anthropic API driver.

`mamba auth` / `mamba refresh` still exist for that path: they pull the
OAuth access token from the macOS keychain (`Claude Code-credentials`),
auto-exchange via Anthropic's OAuth endpoint if expired, and write to
`~/.mamba/runtime.env`. Anthropic's OAuth endpoint rate-limits to ~3
req/min — `mamba refresh` retries 3× with 30s/60s/120s backoff.

## Agents

mamba routes tasks by agent slug. The expected slugs are defined in
`open-mamba/agents/*.toml`. Each has a corresponding openfang manifest in
`open-mamba/agents/openfang-manifests/*.toml`. `mamba up` (and `mamba
register-agents`) spawn any missing ones.

| mamba slug      | openfang agent  | default model            |
|-----------------|-----------------|--------------------------|
| coder           | coder           | claude-sonnet-4-20250514 |
| code-reviewer   | code-reviewer   | claude-sonnet-4-20250514 |
| taifoon-intel   | taifoon-intel   | nemotron/taifoon (direct, bypasses openfang) |

The `model` field in a `TaskEnvelope` overrides per-task. Submit
`claude-opus-4-7` to escalate.

## Submitting a task

```bash
curl -X POST http://localhost:1337/ingest \
  -H 'content-type: application/json' \
  -d '{
    "project":      "spinner",
    "assigned_agent": "coder",
    "skill":        "simplify",
    "model":        "claude-opus-4-7",
    "payload":      "fix the bug at file.rs:42",
    "priority":     1,
    "source":       "claude_opus"
  }'
```

mamba returns `{"id": "<uuid>"}`. Track with `GET /api/tasks/<id>`.

## Files

- `scripts/mamba` — this script (symlinked to `/opt/homebrew/bin/mamba`)
- `~/.mamba/runtime.env` — auth env loaded by both daemons
- `~/.mamba/logs/open-mamba.log` — mamba server log
- `~/.mamba/logs/openfang.log` — openfang daemon log
- `~/.mamba/{open-mamba,openfang}.pid` — pid files for `down`/`status`
- `agents/openfang-manifests/*.toml` — agent definitions for openfang
