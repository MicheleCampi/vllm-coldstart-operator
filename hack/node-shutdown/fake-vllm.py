#!/usr/bin/env python3
"""A stand-in for `vllm serve` in the node-shutdown job (ADR-0028): the
OpenAI routes the operator's probes call (/health, /v1/models,
/v1/completions), a 100 ms completion, and the worst case on SIGTERM: exit at
once, as the llm-d simulator does (ADR-0019 fact 4). The operator's command
line (vllm serve MODEL --host ... --port 8000 ...) is accepted and ignored,
except the model name."""
import json, os, signal, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODEL = sys.argv[2] if len(sys.argv) > 2 and sys.argv[1] == "serve" else "fake"


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, code, body):
        b = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        if self.path == "/health":
            self._send(200, {})
        elif self.path == "/v1/models":
            self._send(200, {"data": [{"id": MODEL}]})
        else:
            self._send(404, {})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        self.rfile.read(n)
        if self.path != "/v1/completions":
            return self._send(404, {})
        time.sleep(0.1)
        self._send(200, {"choices": [{"text": " ok"}], "pod": os.environ.get("HOSTNAME", "")})

    def log_message(self, *a):
        pass


signal.signal(signal.SIGTERM, lambda *_: os._exit(0))
ThreadingHTTPServer(("0.0.0.0", 8000), H).serve_forever()
