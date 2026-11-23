"""The client of the node-shutdown job: one completion every 0.2 s through
the Service, a new connection each time, 5 s timeout, no retry. One JSON line
per request on stdout: {"t": unix time, "code": HTTP code or 0, "s": seconds,
"pod": the pod that answered}."""
import json, sys, time, urllib.request as u

url = sys.argv[1]
body = json.dumps({"model": "m", "prompt": "hi", "max_tokens": 1}).encode()
while True:
    t0 = time.time()
    code, pod = 0, ""
    try:
        r = u.urlopen(u.Request(url, data=body, headers={"Content-Type": "application/json"}), timeout=5)
        code, pod = r.status, json.loads(r.read()).get("pod", "")
    except Exception as e:  # refused, reset, timeout: the request failed
        code = getattr(e, "code", 0)
    print(json.dumps({"t": round(t0, 3), "code": code, "s": round(time.time() - t0, 3), "pod": pod}), flush=True)
    time.sleep(max(0.0, 0.2 - (time.time() - t0)))
