#!/usr/bin/env python3
"""COIN-SPLIT final sequence: 6 drains to the z2 (DNS2-only) destinations,
then the residue drain to the PC zone. Sequential, zero-change, mine-waits."""
import json, re, time, urllib.request

RPC = "http://127.0.0.1:19112"
SEED_ADDRS = [
    "TSt5LsVXdc332z1Q2CyQJJ6EZwnJwKwxTx", "TAEM6CbP8Mxusq6Pb1b3zDEBnaQaXD1iJq",
    "TT3sGw6oM89J3XhVQAjfwAvm47jVceAkpN", "TTf2udT4DW64Do3LEnf9RUQoNatiHANnQw",
    "THqUXnZWjbCamggMs8YUsipX4KhXchDPo2", "TUDiACrYTPhYkhp1Yaa8W8gndnkyLWKuBm",
    "TRsiRzkMWm87ZuWFwPB8YXFGYr5AQZo7fb", "TAHtDT9n5UZuD953vqWiXs9eBWFLwwCosg",
    "TBWLnkuXmGBeYbRQxhvXSLrfj3YtFftoGL", "TBzTNFQgN2sKHpZp3h1ev9jnx5S21YnxDM",
]
SENDS = [
    ("TVVcEE95oXp9trwdGBRscedKgXeTqCZiJT", "z2_1"),
    ("TVkRs3MFRZbaqFu69rxWYuF69Tf64kPRaT", "z2_2"),
    ("TE5AnHMj9i15KYk6tW4Ztk6FobjwWYkGc8", "z2_3"),
    ("TQYsQtEdSCoM5rS1Js1fK9tHnoXtrCJWvZ", "z2_4"),
    ("TGNJduic9bvTF8Btdz2iBfbk8LdUTXWw4N", "z2_5"),
    ("TVVcEE95oXp9trwdGBRscedKgXeTqCZiJT", "z2_1b"),
    ("TA5SHCmj3BQbMcRNcYP5F1RjP7ZWEVCBvs", "pc_residue"),
]
LOG = "/root/tri7-lab/split_log.jsonl"
DEADLINE = time.time() + 42 * 60

def rpc(method, params=None, timeout=60):
    body = json.dumps({"method": method, "params": params or []}).encode()
    req = urllib.request.Request(RPC, data=body)
    r = json.load(urllib.request.urlopen(req, timeout=timeout))
    if r.get("error"):
        raise RuntimeError(f"{method}: {r['error']}")
    return r["result"]

def log(obj):
    obj["utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    with open(LOG, "a") as f:
        f.write(json.dumps(obj) + "\n")
    print(json.dumps(obj), flush=True)

def seed_total():
    lu = rpc("listunspent", [], 60)
    per = {}
    for row in lu:
        a = row.get("address")
        if a in SEED_ADDRS:
            per[a] = per.get(a, 0) + int(round(row["amount"] * 1_000_000))
    return {a: b for a, b in per.items() if b > 2000}

def converge(dest, start=300_000_000):
    amt = start
    for _ in range(14):
        try:
            d = rpc("sendtoaddress", [dest, amt / 1_000_000, "dryrun"])
        except RuntimeError as e:
            m = re.search(r"have (\d+), need (\d+)", str(e))
            if not m:
                raise
            have, need = int(m.group(1)), int(m.group(2))
            amt = amt - (need - have) - 500
            continue
        if not isinstance(d, dict):
            raise RuntimeError(f"dryrun gave {type(d).__name__}: {repr(d)[:120]}")
        if d["change_sats"] == 0:
            return amt, d["fee_sats"]
        amt += d["change_sats"]
    raise RuntimeError("no convergence")

def wait_mined(txid, timeout_s=480):
    t0 = time.time()
    while time.time() - t0 < timeout_s:
        if txid not in rpc("getrawmempool", [], 15):
            return True
        time.sleep(10)
    return False

def main():
    log({"event": "final-seq-start", "tip": rpc("getblockcount"), "seed": seed_total()})
    for dest, tag in SENDS:
        if time.time() > DEADLINE:
            log({"event": "deadline", "tag": tag}); return
        rows = seed_total()
        if not rows:
            log({"event": "seed-empty", "tag": tag}); break
        last = tag == "pc_residue"
        try:
            amt, fee = converge(dest, start=90_000_000 if last else 300_000_000)
            txid = rpc("sendtoaddress", [dest, amt / 1_000_000])
            if not isinstance(txid, str):
                txid = txid.get("txid") if isinstance(txid, dict) else None
            log({"event": "sent", "tag": tag, "dest": dest, "amount_sats": amt,
                 "fee_sats": fee, "txid": txid})
            mined = wait_mined(txid, 480)
            log({"event": "mined" if mined else "mine-timeout", "tag": tag, "txid": txid,
                 "tip": rpc("getblockcount")})
        except Exception as e:
            log({"event": "error", "tag": tag, "dest": dest, "err": str(e)[:220]})
            time.sleep(30)
    time.sleep(10)
    lu = rpc("listunspent", [], 60)
    zones = {}
    for row in lu:
        zones[row["address"]] = zones.get(row["address"], 0) + row["amount"]
    log({"event": "final-seq-done", "seed": seed_total(),
         "dns2_balance_tri": rpc("getbalance"),
         "zone_balances": {k: round(v, 6) for k, v in sorted(zones.items(), key=lambda kv: -kv[1])[:24]},
         "tip": rpc("getblockcount")})

if __name__ == "__main__":
    main()
