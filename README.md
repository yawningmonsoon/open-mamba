# open-mamba

**Autonomous task bus for delivering work to Claude / nemotron agents.**

You drop work into a queue (DuckDB). A background worker picks each task up
and dispatches it to the right agent runtime — Claude (via the local
[`claude`](https://github.com/anthropics/claude-code) CLI through openfang),
or nemotron at scanner.taifoon.dev. No API keys to juggle, no cron loops to
debug, no copy-paste between terminals.

```
                        ┌──────────────────┐
   POST /ingest    ────►│   open-mamba     │  ◄── DuckDB lake (durable queue)
                        │   :1337          │
                        │   ┌──────────┐   │
                        │   │  worker  │   │  polls pending, max 4 in-flight
                        │   └────┬─────┘   │
                        └────────┼─────────┘
                                 │
                ┌────────────────┴────────────────┐
                ▼                                 ▼
        ┌────────────┐                    ┌────────────┐
        │  openfang  │ ──► claude -p ─►   │  nemotron  │ ──► scanner.taifoon.dev
        │  :4200     │     (local CLI,    │  (HTTP)    │     (taifoon/polymarket
        │            │      your auth)    │            │      /algotrada adapters)
        └────────────┘                    └────────────┘
```

---

## One-liner setup

```bash
# Prereqs (one-time):
brew install gh jq           # gh CLI + jq are required
gh auth login                # log into your GitHub account
claude /login                # log into Claude Code (Pro/Max/Team plan)

# Build + symlink the mamba CLI:
cd ~/projects/open-mamba && cargo build --bin open-mamba
cd ~/projects/openfang   && cargo build --release --bin openfang   # ~10 min first time
ln -sf ~/projects/open-mamba/scripts/mamba /opt/homebrew/bin/mamba

# Boot the autonomous bus:
mamba up
```

That's it. `mamba up` brings up everything, registers the agents, and
smoke-tests dispatch. After that, `POST /ingest` sends Claude tasks.

---

## Daily commands

```bash
mamba up                 # start everything (idempotent — safe to re-run)
mamba status             # running pids, ports, queue depth, recent costs
mamba logs               # tail open-mamba + openfang logs
mamba down               # stop both daemons (alias: stop)

mamba retry-pending      # re-dispatch any stuck tasks (rare; use after a crash)
mamba retry <task-id>    # re-dispatch one task by ID

mamba register-agents    # spawn missing agents (run after editing manifests)
mamba auth | refresh     # legacy: pull OAuth from keychain (not needed for default flow)
```

---

## Submitting work

```bash
curl -X POST http://localhost:1337/ingest \
  -H 'content-type: application/json' \
  -d '{
    "project":        "spinner",
    "assigned_agent": "coder",
    "skill":          "simplify",
    "model":          "claude-opus-4-7",
    "payload":        "Read /Users/me/repo/CLAUDE.md, summarize the deploy section in 100 words.",
    "priority":       1,
    "source":         "claude_opus"
  }'
# → {"id":"3746520a-105b-4b7f-9777-446dfe6033ff"}
```

The worker picks it up within ~2 seconds. Track with:

```bash
curl -s http://localhost:1337/api/tasks/3746520a-105b-4b7f-9777-446dfe6033ff
# → {"status":"done","cost_usd":0.0252,"tokens_out":1005, ...}
```

---

## How auth works

**You don't manage Anthropic credentials.** Tasks routed to a Claude model
go through openfang's `claude-code` provider, which shells out to the
`claude` CLI on your `$PATH`. Whatever auth `claude /login` set up,
mamba inherits.

| Where auth lives | What it covers |
|---|---|
| `claude /login` (macOS keychain) | All Claude tasks (opus, sonnet, haiku) |
| `gh auth login` (macOS keychain via gh) | Git push, GitHub API, PR creation |
| `.env` / `~/.mamba/runtime.env` | Optional: `ANTHROPIC_API_KEY` for direct API path |

`mamba up` deliberately **strips `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN`
from the openfang environment** because the `claude` CLI prefers env-var
keys over its own keychain session — a stale env var causes 401s even when
your `claude /login` is healthy.

If you'd rather use a real API key (skips Claude Code entirely):
1. `echo 'ANTHROPIC_API_KEY=sk-ant-api03-…' >> ~/.mamba/runtime.env`
2. Submit tasks with `model: "claude-sonnet-4-20250514"` (the bus's
   model mapper passes literal `claude-*-NNN` strings through to the
   standard Anthropic driver, bypassing the claude-code shim).

---

## GitHub identity for agents

Tasks that do git work (commits, PRs, pushes) inherit the machine's
existing `gh` + git config. To keep the *commit author* and the *push
identity* aligned with whatever account `gh auth login` is using, run:

```bash
mamba git-init                # in any repo where an agent will commit
```

That wrapper:

1. Reads the active gh login (`gh api user --jq .login`).
2. Sets `git config user.name` and `user.email` to the gh-provided
   identity (`<login>@users.noreply.github.com` — GitHub's privacy
   forwarder, never reveals a real address).
3. Runs `gh auth setup-git` so HTTPS pushes use the gh-stored token
   as the credential helper.

After that, any agent (or you) who commits in that repo signs as the
same account that pushes — no split identity, nothing personal in the
log.

To inspect what an agent will commit as:

```bash
mamba whoami      # active gh login + configured git committer in $PWD
```

---

## Architecture

### `mamba-api` (port 1337) — the bus
- DuckDB lake at `data/mamba.duckdb` holds all envelopes
- `POST /ingest` writes a row, worker picks it up
- `POST /api/tasks/:id/retry` and `POST /api/tasks/retry-pending` reset
  status back to `pending` for re-dispatch
- Background worker: 2s poll, max 4 concurrent dispatches

### `mamba-bus` — the dispatcher
- `Bus.route` decides nemotron vs openfang based on `model` field
- Models matching `nemotron/*` → direct HTTP to scanner.taifoon.dev
- Everything else → openfang (which then routes to claude-code)
- Token usage and cost are written back to DuckDB on completion

### `openfang` (port 4200) — the agent runtime
- Per-agent TOML manifests at `agents/openfang-manifests/*.toml`
- Each agent has a fixed model (manifest-level — openfang's cron
  ignores per-task model overrides, so model = agent identity)
- Default agents: `coder` (opus), `code-reviewer` (sonnet),
  `taifoon-intel` (nemotron), `assistant` (default)

### `claude-code` provider in openfang
- Defined in `openfang/crates/openfang-runtime/src/drivers/claude_code.rs`
- Spawns `claude -p '<prompt>' --dangerously-skip-permissions
  --output-format json --model <opus|sonnet|haiku>` per turn
- Uses your local CLI session — no API key in env

---

## File locations

| Path | What |
|---|---|
| `~/projects/open-mamba/` | This repo (bus, lake, worker, scripts) |
| `~/projects/openfang/` | Agent runtime (separate repo, has the OAuth patch) |
| `/opt/homebrew/bin/mamba` | Symlink → `scripts/mamba` |
| `~/.mamba/runtime.env` | Auto-generated env loaded by both daemons |
| `~/.mamba/logs/` | open-mamba.log + openfang.log |
| `~/.mamba/{open-mamba,openfang}.pid` | Pid files for `mamba down/status` |
| `~/.openfang/data/openfang.db` | openfang's session/agent SQLite store |
| `~/projects/open-mamba/data/mamba.duckdb` | The task lake |
| `~/projects/open-mamba/agents/openfang-manifests/` | Agent definitions |

---

## Patched dependencies

This setup ships two upstream patches that are not yet merged:

1. **`openfang/crates/openfang-runtime/src/drivers/anthropic.rs`** — adds
   OAuth Bearer auth detection (`sk-ant-oat` prefix → `Authorization:
   Bearer` + `anthropic-beta: oauth-2025-04-20`). Currently unused since
   we route through `claude-code`, but available as a fallback.

2. **`open-mamba/crates/mamba-bus/src/openfang.rs`** — replaced cron-based
   dispatch with direct `POST /api/agents/{id}/message`. The cron path
   re-fired tasks on every tick because openfang's `compute_next_run` for
   `CronSchedule::At` returns the same scheduled time forever. Direct
   dispatch + the queue worker is the proper one-shot pattern.

---

## Public deployment hardening

This repo is safe to publish — the build runs without secrets, and CI
includes a [gitleaks](https://github.com/gitleaks/gitleaks) scan
(`.github/workflows/secret-scan.yml`) that fails any push or PR
introducing a credential. But running the **bus itself** publicly
without auth would let anyone spend your nemotron / claude budget. The
checklist:

1. **Set `MAMBA_API_KEY`** before exposing the server beyond localhost:

   ```bash
   echo "MAMBA_API_KEY=$(openssl rand -hex 32)" >> ~/projects/open-mamba/.env
   ```

   When set, `/ingest`, `/api/tasks/*/retry`, and
   `/api/nemotron/*/generate` require `Authorization: Bearer <key>` (or
   `x-mamba-key: <key>`). Read endpoints (`/api/tasks`, `/api/analytics/*`,
   `/health`, `/api/nemotron/health`) stay open so dashboards work.

2. **Bind to a private interface** unless you're behind a TLS-terminating
   reverse proxy: set `BIND=127.0.0.1` rather than the default `0.0.0.0`.

3. **Don't expose openfang directly** — it has no auth at all. Keep it on
   localhost; `mamba up` configures it that way (`api_listen = 127.0.0.1:4200`).

4. **Rotate `MAMBA_ENCRYPT_KEY`** if you've used the all-zeroes
   placeholder from `.env.example`. The audit-log encryption depends on
   it being secret.

5. **No private chat IDs in code.** The `MAMBA_TELEGRAM_CHAT` var is
   blank by default — set it locally only if you want delivery to a
   personal chat. Don't commit a populated `.env`.

6. **CI runs unauthenticated.** Workflows in `.github/workflows/` request
   only `contents: read`. They never need API keys, GitHub PATs, or any
   secret to validate a PR.

If you want this repo backed up on GitHub under your active `gh` account:

```bash
cd ~/projects/open-mamba
gh repo create open-mamba --public --source=. --remote=origin --push
```

That uses the credential helper that `gh auth login` already configured.
No tokens to copy. Same applies to a fork of openfang if you want to
preserve the OAuth patch.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `mamba refresh` returns 429 | Anthropic OAuth endpoint rate limit (~3 req/min) | Wait 5 min, try again |
| Task stuck in `dispatched` | Worker started after task came in | `mamba retry <id>` |
| Task fails with `Invalid API key` | `ANTHROPIC_API_KEY` set in environment | `unset ANTHROPIC_API_KEY && mamba down && mamba up` |
| `claude CLI failed` empty stderr | `claude` binary not in openfang's PATH | Verify `which claude` and `mamba down && mamba up` |
| `openfang agent 'X' not found` | Manifest exists but agent not spawned | `mamba register-agents` |
| Commits attributed to wrong user | Local `git config user.email` differs from gh | `git config user.email <correct-email>` per repo |

For deeper debugging: `mamba logs` tails both daemons live.

---

## Roadmap

- [ ] Overseer agent using Claude `/loop` dynamic wakeup — watches queue
      depth, retries failures, summarises completions on its own cadence.
- [ ] Webhook completion delivery (Telegram, Slack, email).
- [ ] Per-agent token budgets enforced at dispatch time.
- [ ] Replay log so a worker restart never loses an in-flight turn.
