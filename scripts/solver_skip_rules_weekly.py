#!/usr/bin/env python3
"""
X1: weekly skip-rule synthesis from solver outcomes.

Pull every outcome from the lake, group by protocol, ask nemotron to propose
JSON skip-rules, validate, then POST the active set back to mamba so the solver
picks them up at next restart.

Designed to be invoked by `cron` or `mamba ingest`. Idempotent — calling it
twice in a row produces the same rule set (assuming the outcomes haven't
changed). On underfit data (< MIN_SAMPLES_PER_PROTOCOL) the script publishes
NO rules for that protocol, intentionally — better no rule than a noisy one.

Env:
  MAMBA_URL          (default http://localhost:1337)
  MAMBA_API_KEY      (only needed when the bus has it set; protected endpoint)
  NEMOTRON_MODEL     (default nemotron/taifoon)
  MIN_SAMPLES_PER_PROTOCOL  (default 50; under this we publish nothing)
"""
from __future__ import annotations

import json
import os
import sys
import urllib.request
import urllib.error
from collections import defaultdict
from typing import Any

MAMBA_URL = os.environ.get("MAMBA_URL", "http://localhost:1337").rstrip("/")
MAMBA_API_KEY = os.environ.get("MAMBA_API_KEY", "").strip()
NEMOTRON_MODEL = os.environ.get("NEMOTRON_MODEL", "nemotron/taifoon")
MIN_SAMPLES = int(os.environ.get("MIN_SAMPLES_PER_PROTOCOL", "50"))
OUTCOME_PAGE = int(os.environ.get("OUTCOME_PAGE", "5000"))

# Defensive bounds on rule values. Anything outside these is considered noise
# from an underfit prompt and dropped.
MAX_AMOUNT_USD = 100_000.0
MAX_GAS_GWEI   = 10_000.0


def http(method: str, path: str, body: dict | None = None) -> Any:
    url = f"{MAMBA_URL}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    if data is not None:
        req.add_header("content-type", "application/json")
    if MAMBA_API_KEY:
        req.add_header("authorization", f"Bearer {MAMBA_API_KEY}")
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        print(f"  ! HTTP {e.code} {url}: {e.read()[:200]!r}", file=sys.stderr)
        raise


def fetch_outcomes() -> list[dict]:
    return http("GET", f"/api/solver/outcomes?limit={OUTCOME_PAGE}") or []


def summarize(rows: list[dict]) -> dict[str, dict]:
    """Per-protocol rollup: counts, skip rate, profit distribution."""
    groups: dict[str, list[dict]] = defaultdict(list)
    for r in rows:
        groups[r.get("protocol", "?")].append(r)

    summary = {}
    for proto, items in groups.items():
        executed = [r for r in items if r.get("decision", "").startswith("executed")]
        skipped  = [r for r in items if r.get("decision", "").startswith("skip")]
        actual_profits = [r["actual_profit_usd"] for r in executed
                          if isinstance(r.get("actual_profit_usd"), (int, float))]
        predicted = [r["predicted_profit_usd"] for r in items
                     if isinstance(r.get("predicted_profit_usd"), (int, float))]
        gas_used = [r["actual_gas"] for r in executed
                    if isinstance(r.get("actual_gas"), int) and r["actual_gas"] > 0]
        summary[proto] = {
            "n_total":     len(items),
            "n_executed":  len(executed),
            "n_skipped":   len(skipped),
            "actual_profit_min": min(actual_profits) if actual_profits else None,
            "actual_profit_p50": _pct(actual_profits, 0.50),
            "actual_profit_p90": _pct(actual_profits, 0.90),
            "predicted_profit_p50": _pct(predicted, 0.50),
            "gas_used_p50": _pct(gas_used, 0.50),
            "gas_used_p90": _pct(gas_used, 0.90),
        }
    return summary


def _pct(xs: list[float], q: float) -> float | None:
    if not xs:
        return None
    s = sorted(xs)
    i = max(0, min(len(s) - 1, int(q * len(s))))
    return s[i]


SYSTEM_PROMPT = """You are a quantitative analyst for an autonomous cross-chain solver.

Given a JSON summary of recent solver outcomes, propose AT MOST 2 skip-rules
per protocol that would have improved net profitability without skipping any
clearly-profitable fill.

Each rule is a JSON object with optional fields:
  - min_amount_usd  (skip if intent amount in USD is BELOW this)
  - max_gas_gwei    (skip if current gas price in gwei is ABOVE this)
  - dst_chain       (apply rule only on this destination chain id)

Output STRICT JSON: {"rules":[{"rule":{...},"description":"...","confidence":0.0-1.0}]}.
Do not output any prose. If the data is too thin to support a rule, return
{"rules":[]}.

Conservative bias: prefer FEWER rules with HIGHER confidence. Never propose a
rule that would have skipped > 25% of executed fills.
"""


def propose_rules(protocol: str, stats: dict) -> list[dict]:
    prompt = json.dumps({"protocol": protocol, "stats": stats})
    body = {"prompt": prompt, "system": SYSTEM_PROMPT, "max_tokens": 512}
    try:
        resp = http("POST", f"/api/nemotron/{NEMOTRON_MODEL}/generate", body)
    except Exception as e:
        print(f"  ! nemotron call failed for {protocol}: {e}", file=sys.stderr)
        return []
    text = resp.get("text") or resp.get("output") or ""
    text = text.strip()
    # Best-effort JSON extraction
    try:
        start = text.index("{")
        end   = text.rindex("}") + 1
        parsed = json.loads(text[start:end])
    except (ValueError, json.JSONDecodeError):
        print(f"  ! nemotron returned non-JSON for {protocol}: {text[:120]!r}", file=sys.stderr)
        return []
    rules = parsed.get("rules", [])
    return validate_rules(protocol, rules, stats)


def validate_rules(protocol: str, rules: list[dict], stats: dict) -> list[dict]:
    n_executed = stats.get("n_executed", 0)
    out = []
    for entry in rules:
        rule = entry.get("rule") or entry
        desc = entry.get("description")
        conf = float(entry.get("confidence", 0.0))
        if not isinstance(rule, dict):
            continue
        # Bounds
        if "min_amount_usd" in rule:
            v = rule["min_amount_usd"]
            if not isinstance(v, (int, float)) or v <= 0 or v > MAX_AMOUNT_USD:
                continue
        if "max_gas_gwei" in rule:
            v = rule["max_gas_gwei"]
            if not isinstance(v, (int, float)) or v <= 0 or v > MAX_GAS_GWEI:
                continue
        if "dst_chain" in rule:
            v = rule["dst_chain"]
            if not isinstance(v, int) or v <= 0:
                continue
        if not any(k in rule for k in ("min_amount_usd", "max_gas_gwei", "dst_chain")):
            continue
        if not 0.0 <= conf <= 1.0:
            conf = 0.5
        out.append({
            "rule_json":   json.dumps(rule, sort_keys=True),
            "description": desc,
            "confidence":  conf,
            "sample_size": n_executed,
        })
    # Cap at 2 rules per protocol per spec
    return out[:2]


def publish(protocol: str, rules: list[dict]) -> None:
    body = {"protocol": protocol, "rules": rules}
    resp = http("POST", "/api/solver/skip-rules", body)
    print(f"  → published {len(rules)} rule(s) for {protocol}: {resp}")


def main() -> int:
    print(f"📥 fetching outcomes from {MAMBA_URL} ...")
    rows = fetch_outcomes()
    print(f"   {len(rows)} rows")
    if not rows:
        print("nothing to learn from yet — exiting cleanly")
        return 0

    summary = summarize(rows)
    print(f"📊 protocols seen: {list(summary)}")

    for protocol, stats in summary.items():
        n = stats["n_total"]
        if n < MIN_SAMPLES:
            print(f"  · {protocol}: {n} samples < {MIN_SAMPLES} threshold — skipping rule synthesis")
            # Publish an EMPTY set so any stale rules from a smaller dataset are
            # superseded. This is the safe default.
            publish(protocol, [])
            continue
        print(f"  · {protocol}: {n} samples — asking nemotron")
        proposed = propose_rules(protocol, stats)
        publish(protocol, proposed)

    return 0


if __name__ == "__main__":
    sys.exit(main())
