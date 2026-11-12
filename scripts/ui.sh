#!/usr/bin/env bash
# Local SpendGuard console (policy + approve/reject log). No network required.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
PORT="${PORT:-8787}"
HOST="${HOST:-127.0.0.1}"
echo "Opening SpendGuard console on http://${HOST}:${PORT}"
echo "Press Ctrl+C to stop."
exec python3 tools/agent/server.py --host "$HOST" --port "$PORT"
