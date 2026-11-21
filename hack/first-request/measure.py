"""Time completions against a vLLM OpenAI server, one JSON line each.

Usage: measure.py BASE_URL MODEL LABEL:MAX_TOKENS:PROMPT ...
Prints {"label", "max_tokens", "code", "s"} per request, in order, so the
first line is the first request the server ever answered after Ready.
"""
import json
import sys
import time
import urllib.request

base, model = sys.argv[1], sys.argv[2]
for spec in sys.argv[3:]:
    label, max_tokens, prompt = spec.split(":", 2)
    body = json.dumps({"model": model, "prompt": prompt, "max_tokens": int(max_tokens)}).encode()
    req = urllib.request.Request(f"{base}/v1/completions", data=body, headers={"Content-Type": "application/json"})
    t0 = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            code = r.status
            r.read()
    except Exception as e:  # noqa: BLE001 - recorded, not raised
        code = getattr(e, "code", 0)
    print(json.dumps({"label": label, "max_tokens": int(max_tokens), "code": code, "s": round(time.monotonic() - t0, 2)}), flush=True)
