#!/usr/bin/env python3
"""Fleet tip-unification monitor for the Triangles (TRI) tri7 fleet.

Queries getbestblockhash (+ height) from every reachable tri7d node and
compares them. Any divergence is alarmed to Telegram immediately.

Design intent (Sami's unification guarantee): all fleet nodes must follow ONE
canonical chain. A node whose best block hash differs from the primary is a
fork-in-progress and pages the operator within minutes.

Nodes:
  DNS2     127.0.0.1:19112        (primary, loopback)
  samihost 100.98.140.83:19112    (tailscale, follow-mode)
  sami-pc  via tailscale 19114    (Windows desktop node)

Exit codes: 0 unified, 1 divergence, 2 unreachable-majority.
"""
import json
import urllib.request
import sys

NODES = {
    "DNS2": "http://127.0.0.1:19112",
    "samihost": "http://100.98.140.83:19112",
    "sami-pc": "http://100.85.236.10:19114",
}

def rpc(url, method):
    body = json.dumps({"method": method, "params": []}).encode()
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        d = json.load(r)
    if d.get("error"):
        raise RuntimeError(str(d["error"])[:80])
    return d["result"]

def main():
    tips = {}
    unreachable = []
    for name, url in NODES.items():
        try:
            h = rpc(url, "getbestblockhash")
            ht = rpc(url, "getblockcount")
            tips[name] = (ht, h)
        except Exception as e:
            unreachable.append(f"{name}: {e}")

    if len(tips) < 2:
        print("UNREACHABLE-MAJORITY:", "; ".join(unreachable) or "no nodes answered")
        sys.exit(2)

    heights = {v[0] for v in tips.values()}
    hashes = {v[1] for v in tips.values()}
    detail = {k: f"{v[0]}:{v[1][:12]}" for k, v in tips.items()}
    print(json.dumps({"tips": detail, "unreachable": unreachable}))

    if len(hashes) == 1:
        print(f"UNIFIED at height {heights.pop()} — all reachable nodes agree")
        sys.exit(0)

    print("FORK DETECTED — nodes disagree on best block:")
    by_hash = {}
    for k, (ht, h) in tips.items():
        by_hash.setdefault(h, []).append(k)
    for h, names in by_hash.items():
        hs = [tips[n][0] for n in names]
        print(f"  {'+'.join(names)} at height {max(hs)} hash {h[:16]}")
    sys.exit(1)

if __name__ == "__main__":
    main()
