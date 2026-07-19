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
state = {"hits": 0.0, "queries": 0.0}


class H(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/metrics":
            self.send_response(404)
            self.end_headers()
            return
        state["queries"] += STEP
        state["hits"] += STEP * RATIO
        half = state["hits"] / 2.0
        body = (
            "# HELP vllm:prefix_cache_queries q\n"
            "# TYPE vllm:prefix_cache_queries counter\n"
            f'vllm:prefix_cache_queries{{model="fixture"}} {state["queries"]}\n'
            "# HELP vllm:prefix_cache_hits h\n"
            "# TYPE vllm:prefix_cache_hits counter\n"
            f'vllm:prefix_cache_hits{{model="fixture",shard="0"}} {half}\n'
            f'vllm:prefix_cache_hits{{model="fixture",shard="1"}} {half}\n'
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; version=0.0.4")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


HTTPServer(("0.0.0.0", 9090), H).serve_forever()
