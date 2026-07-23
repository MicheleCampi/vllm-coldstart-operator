"""Fake vLLM /metrics endpoint for the ADR-0007 real-source rehearsal.

Serves the two ADR-011 series with counters that advance on every scrape,
pinned to HIT_RATIO so the reporter's delta-derived rate is deterministic.
`hits` is split across two label sets on purpose: the reporter's parser
must sum a family across label sets, and the rehearsal should exercise
that on the wire, not only in unit tests.
"""
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

RATIO = float(os.environ.get("HIT_RATIO", "0.5"))
STEP = 10.0
state = {"hits": 0.0, "queries": 0.0, "tokens": 0.0}


class H(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/metrics":
            self.send_response(404)
            self.end_headers()
            return
        state["queries"] += STEP
        state["hits"] += STEP * RATIO
        half = state["hits"] / 2.0
        # Mirror the real vLLM v0.23.0 schema (verified live, A10 session
        # 2026-07-22): OpenMetrics `_total` suffix, engine/model_name labels,
        # `_created` gauges alongside that the parser guard must ignore.
        state["tokens"] += STEP * 4
        body = (
            "# HELP vllm:prefix_cache_queries_total q\n"
            "# TYPE vllm:prefix_cache_queries_total counter\n"
            f'vllm:prefix_cache_queries_total{{engine="0",model_name="fixture"}} {state["queries"]}\n'
            f'vllm:prefix_cache_queries_created{{engine="0",model_name="fixture"}} 1.78e9\n'
            "# HELP vllm:prefix_cache_hits_total h\n"
            "# TYPE vllm:prefix_cache_hits_total counter\n"
            f'vllm:prefix_cache_hits_total{{engine="0",model_name="fixture",shard="0"}} {half}\n'
            f'vllm:prefix_cache_hits_total{{engine="0",model_name="fixture",shard="1"}} {half}\n'
            f'vllm:prefix_cache_hits_created{{engine="0",model_name="fixture"}} 1.78e9\n'
            "# HELP vllm:generation_tokens_total t\n"
            "# TYPE vllm:generation_tokens_total counter\n"
            f'vllm:generation_tokens_total{{engine="0",model_name="fixture"}} {state["tokens"]}\n'
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; version=0.0.4")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


HTTPServer(("0.0.0.0", 9090), H).serve_forever()
