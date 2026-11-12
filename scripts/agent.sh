#!/usr/bin/env bash
# Run the purchasing agent against the local policy engine.
# Default: deterministic offline fallback. Set SPENDGUARD_LLM_API_KEY
# (or OPENAI_API_KEY / NEAR_AI_API_KEY) plus an optional
# SPENDGUARD_LLM_BASE_URL (default https://cloud-api.near.ai/v1) to use
# an OpenAI-compatible model. The policy is enforced either way.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
exec python3 tools/agent/agent.py "$@"
