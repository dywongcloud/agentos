#!/usr/bin/env python3
"""Minimal stand-in for `holo serve`'s /health and agent-card endpoints, just
enough for HoloBridge::start()'s startup probe (wait_for_health +
probe_agent_card) to succeed, so the control-channel wiring in main.rs can be
witnessed end-to-end without the real hcompai/holo-desktop-cli installed."""
import http.server
import json
import sys

PORT = int(sys.argv[sys.argv.index("--port") + 1]) if "--port" in sys.argv else 8765

class Handler(http.server.BaseHTTPRequestHandler):
    def _json(self, obj, status=200):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            self._json({"service": "holo-desktop", "status": "ok", "version": "0.0.0-fake"})
        elif self.path == "/.well-known/agent-card.json":
            self._json({
                "capabilities": {"streaming": True, "pushNotifications": False},
                "supportedInterfaces": [{"url": f"http://127.0.0.1:{PORT}/a2a", "protocolBinding": "JSONRPC", "protocolVersion": "0.3.0"}],
            })
        else:
            self._json({"error": "not found"}, status=404)

    def log_message(self, fmt, *args):
        pass  # keep stderr quiet; this is a test double, not a real bridge

if __name__ == "__main__":
    print(f"fake holo serve · v0.0.0-fake", file=sys.stderr)
    print(f"  http://127.0.0.1:{PORT}/a2a", file=sys.stderr)
    print("  Ctrl+C to stop", file=sys.stderr)
    http.server.HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
