#!/usr/bin/env python3
"""COIN-SPLIT follow-up v3: identical to v2 but gates each converge on an
EMPTY relay pool (a pending tx's coins conflict for the next send), and the
mine-wait is deadline-capped rather than fixed-480s so mint lulls don't
cascade into -25 errors."""
import json, re, subprocess, time, urllib.request

RPC = "http://127.0.0.1:19112"
SEED_ADDRS = [
    "TSt5LsVXdc332z1Q2CyQJJ6EZwnJwKwxTx", "TAEM6CbP8Mxusq6Pb1b3zDEBnaQaXD1iJq",
    "TT3sGw6oM89J3XhVQAjfwAvm47jVceAkpN", "TTf2udT4DW64Do3LEnf9RUQoNatiHANnQw",
    "THqUXnZWjbCamggMs8YUsipX4KhXchDPo2", "TUDiACrYTPhYkhp1Yaa8W8gndnkyLWKuBm",
    "TRsiRzkMWm87ZuWFwPB8YXFGYr5AQZo7fb", "TAHtDT9n5UZuD953vqWiXs9eBWFLwwCosg",
    "TBWLnkuXmGBeYbRQxhvXSLrfj3YtFftoGL", "TBzTNFQgN2sKHpZp3h1ev9jnx5S21YnxDM",
]
Z2_DESTS = ["TVVcEE95oXp9trwdGBRscedKgXeTqCZiJT", "TVkRs3MFRZbaqFu69rxWYuF69Tf64kPRaT",
            "TE5AnHMj9i15KYk6tW4Ztk6FobjwWYkGc8", "TQYsQtEdSCoM5rS1Js1fK9tHnoXtrCJWvZ",
            "TGNJduic9bvTF8Btdz2iBfbk8LdUTXWw4N"]
LOG = "/root/tri7-lab/split_log.jsonl"
DEADLINE = time.time() + 45 * 60
FLOOR = 3000

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
    seed = {}
    for row in lu:
        a = row.get("address")
        if a in SEED_ADDRS:
            seed[a] = seed.get(a, 0) + int(round(row["amount"] * 1_000_000))
    return {a: b for a, b in seed.items() if b > FLOOR}

def converge(dest):
    amt = 300_000_000
    for _ in range(16):
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
            raise RuntimeError(f"dryrun gave {type(d).__name__}")
        if d["change_sats"] == 0:
            return amt, d["fee_sats"]
        amt += d["change_sats"]
    raise RuntimeError("no convergence")

def main():
    log({"event": "followup3-start", "tip": rpc("getblockcount")})
    n = 10
    while time.time() < DEADLINE:
        # gate: the relay pool must be empty before we build the next spend
        t0 = time.time()
        while time.time() - t0 < DEADLINE - time.time():
            if not rpc("getrawmempool", [], 15):
                break
            time.sleep(15)
        seed = seed_total()
        tot = sum(seed.values())
        log({"event": "cycle", "n": n, "seed_sats": tot, "seed": seed})
        if tot <= FLOOR:
            log({"event": "seed-drained"}); break
        if time.time() > DEADLINE:
            break
        dest = Z2_DESTS[n % len(Z2_DESTS)]
        try:
            amt, fee = converge(dest)
            txid = rpc("sendtoaddress", [dest, amt / 1_000_000])
            if not isinstance(txid, str):
                txid = None
            log({"event": "sent", "n": n, "dest": dest, "amount_sats": amt,
                 "fee_sats": fee, "txid": txid})
            t0 = time.time()
            while time.time() - t0 < (DEADLINE - time.time() - 120):
                if txid and txid not in rpc("getrawmempool", [], 15):
                    log({"event": "mined", "n": n, "txid": txid,
                         "tip": rpc("getblockcount")})
                    break
                time.sleep(15)
        except Exception as e:
            log({"event": "error", "n": n, "err": str(e)[:200]})
            time.sleep(60)
        n += 1
    seed = seed_total()
    log({"event": "followup3-done", "seed_sats": sum(seed.values()), "seed": seed,
         "dns2_balance_tri": rpc("getbalance"), "tip": rpc("getblockcount")})

if __name__ == "__main__":
    main()
