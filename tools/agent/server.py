#!/usr/bin/env python3
"""Local SpendGuard console: static UI + JSON API. No network required.

    python3 tools/agent/server.py --port 8787
    # then open http://127.0.0.1:8787
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from agent import INJECTED_TASK, LEGIT_TASK, run_demo, run_task  # noqa: E402
from policy import fmt_near, parse_near  # noqa: E402
from vault import Vault, demo_vault  # noqa: E402

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "web"))

_lock = threading.Lock()
_vault = demo_vault()


def _reset() -> Vault:
    global _vault
    _vault = demo_vault()
    return _vault


class Handler(BaseHTTPRequestHandler):
    server_version = "SpendGuard/0.1"

    def log_message(self, fmt, *args):  # quieter: still print errors
        if self.command != "GET" or not self.path.startswith(("/assets", "/favicon", "/")):
            sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

    def _send(self, code: int, body, content_type: str = "application/json") -> None:
        if isinstance(body, (dict, list)):
            raw = json.dumps(body).encode()
        elif isinstance(body, str):
            raw = body.encode()
        else:
            raw = body
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(raw)

    def _read_json(self) -> dict:
        length = int(self.headers.get("Content-Length") or 0)
        if not length:
            return {}
        raw = self.rfile.read(length)
        if not raw:
            return {}
        return json.loads(raw.decode())

    def do_OPTIONS(self):  # noqa: N802
        self.send_response(204)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()

    def do_GET(self):  # noqa: N802
        path = urlparse(self.path).path
        if path in ("/api/state", "/api/status"):
            with _lock:
                return self._send(200, _vault.snapshot())
        if path == "/api/health":
            return self._send(200, {"ok": True})
        return self._serve_static(path)

    def do_POST(self):  # noqa: N802
        path = urlparse(self.path).path
        try:
            data = self._read_json()
        except json.JSONDecodeError:
            return self._send(400, {"error": "invalid JSON"})
        try:
            with _lock:
                result = self._dispatch(path, data)
        except ValueError as e:
            return self._send(400, {"error": str(e)})
        except KeyError as e:
            return self._send(404, {"error": f"unknown route {e}"})
        return self._send(200, result)

    def _dispatch(self, path: str, data: dict) -> dict:
        global _vault
        if path == "/api/reset":
            _reset()
            return {"ok": True, "vault": _vault.snapshot()}
        if path == "/api/policy":
            _vault.set_policy(
                data.get("per_tx", data.get("per_tx_max", "1")),
                data.get("daily", data.get("daily_max", "2")),
                data.get("receivers", data.get("allowed_receivers", [])),
                data.get("methods", data.get("allowed_methods", [])),
                agent=data.get("agent"),
            )
            if "paused" in data:
                _vault.policy.paused = bool(data["paused"])
            return {"ok": True, "vault": _vault.snapshot()}
        if path == "/api/pause":
            _vault.policy.paused = bool(data.get("paused", True))
            return {"ok": True, "paused": _vault.policy.paused, "vault": _vault.snapshot()}
        if path == "/api/deposit":
            amt = parse_near(str(data.get("amount", "0")))
            _vault.balance += amt
            return {"ok": True, "vault": _vault.snapshot()}
        if path in ("/api/simulate", "/api/spend"):
            caller = data.get("caller") or _vault.policy.agent
            receiver = data["receiver"]
            amount = parse_near(str(data["amount"]))
            method = data.get("method") or None
            if path == "/api/simulate":
                d = _vault.simulate(caller, receiver, amount, method)
                return {"decision": d.as_dict(), "vault": _vault.snapshot()}
            d = _vault.spend(
                caller, receiver, amount, method,
                memo=str(data.get("memo", "")),
                thought=str(data.get("thought", "")),
                source=str(data.get("source", "human")),
            )
            return {
                "decision": d.as_dict(),
                "verdict": "APPROVE" if d.allowed else "REJECT",
                "vault": _vault.snapshot(),
            }
        if path == "/api/agent":
            task = data.get("task") or LEGIT_TASK
            if data.get("attack"):
                task = INJECTED_TASK
            prefer_llm = not data.get("offline", False)
            payload = run_task(_vault, task, prefer_llm=prefer_llm)
            return payload
        if path == "/api/demo":
            # Fresh vault so the screenshot/demo is deterministic.
            prefer_llm = not data.get("offline", True)
            demo = run_demo(prefer_llm=prefer_llm)
            # Adopt the demo vault as the live one so the UI log matches.
            # Reconstruct from the returned snapshot's log by just keeping the
            # vault that run_demo built — we need a hook. run_demo returns
            # the snapshot, not the object. Re-run against a new vault we keep.
            v = demo_vault()
            from agent import execute, propose

            clean_props, clean_src = propose(LEGIT_TASK, v, prefer_llm=prefer_llm)
            clean_res = execute(v, clean_props, clean_src)
            atk_props, atk_src = propose(INJECTED_TASK, v, prefer_llm=prefer_llm)
            atk_res = execute(v, atk_props, atk_src)
            _vault = v
            return {
                "clean": {"task": LEGIT_TASK, "source": clean_src, "results": clean_res},
                "attack": {"task": INJECTED_TASK, "source": atk_src, "results": atk_res},
                "vault": _vault.snapshot(),
            }
        raise KeyError(path)

    def _serve_static(self, path: str) -> None:
        if path == "/":
            path = "/index.html"
        # no path traversal
        rel = os.path.normpath(path.lstrip("/"))
        full = os.path.abspath(os.path.join(ROOT, rel))
        if not full.startswith(ROOT) or not os.path.isfile(full):
            return self._send(404, {"error": "not found"})
        ext = os.path.splitext(full)[1].lower()
        types = {
            ".html": "text/html; charset=utf-8",
            ".css": "text/css; charset=utf-8",
            ".js": "application/javascript; charset=utf-8",
            ".svg": "image/svg+xml",
            ".png": "image/png",
            ".jpg": "image/jpeg",
            ".ico": "image/x-icon",
        }
        with open(full, "rb") as f:
            body = f.read()
        self._send(200, body, types.get(ext, "application/octet-stream"))


def main(argv=None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8787)
    args = parser.parse_args(argv)
    httpd = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"SpendGuard console  http://{args.host}:{args.port}", flush=True)
    print("Local mock — no testnet calls, no keys required.", flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nbye")
    return 0


if __name__ == "__main__":
    sys.exit(main())
