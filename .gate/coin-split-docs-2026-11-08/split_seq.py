#!/usr/bin/env python3
"""COIN-SPLIT manual sequence: 8 remaining seed-address drains, one real tx
each (no retry storms), zero-change via dryrun iteration, sequential with
mine-waits. Logs RAW rpc responses to diagnose the string-response quirk."""
import json, time, urllib.request

RPC = "http://127.0.0.1:19112"
SEED_ADDRS = [
    "TSt5LsVXdc332z1Q2CyQJJ6EZwnJwKwxTx", "TAEM6CbP8Mxusq6Pb1b3zDEBnaQaXD1iJq",
    "TT3sGw6oM89J3XhVQAjfwAvm47jVceAkpN", "TTf2udT4DW64Do3LEnf9RUQoNatiHANnQw",
    "THqUXnZWjbCamggMs8YUsipX4KhXchDPo2", "TUDiACrYTPhYkhp1Yaa8W8gndnkyLWKuBm",
    "TRsiRzkMWm87ZuWFwPB8YXFGYr5AQZo7fb", "TAHtDT9n5UZuD953vqWiXs9eBWFLwwCosg",
    "TBWLnkuXmGBeYbRQxhvXSLrfj3YtFftoGL", "TBzTNFQgN2sKHpZp3h1ev9jnx5S21YnxDM",
]
SENDS = [
    ("TA9DVnUqdUS3WcvV1tZyEAjwmq3M9sRdtH", "pc2"),
    ("TABe4ay54yvZVHBSL13gfFVfd35S1U1wmD", "pc3"),
    ("TAGKGAiLTFojNS51xZD2cMG1sVLaRxzJXG", "pc4"),
    ("TVVcEE95oXp9trwdGBRscedKgXeTqCZiJT", "z2_1"),
    ("TVkRs3MFRZbaqFu69rxWYuF69Tf64kPRaT", "z2_2"),
    ("TE5AnHMj9i15KYk6tW4Ztk6FobjwWYkGc8", "z2_3"),
    ("TQYsQtEdSCoM5rS1Js1fK9tHnoXtrCJWvZ", "z2_4"),
    ("TGNJduic9bvTF8Btdz2iBfbk8LdUTXWw4N", "z2_5"),
]
LOG = "/root/tri7-lab/split_log.jsonl"
DEADLINE = time.time() + 40 * 60

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

def seed_rows():
    lu = rpc("listunspent", [], 60)
    per = {}
    for row in lu:
        a = row.get("address")
        if a in SEED_ADDRS:
            per.setdefault(a, [0, 0.0])
            per[a][0] += 1
            per[a][1] += int(round(row["amount"] * 1_000_000))
    return {a: v for a, v in per.items() if v[1] > 2000}

def converge(dest):
    amt = 300_000_000  # under any single address pool; the dryrun tells all
    for _ in range(14):
        try:
            d = rpc("sendtoaddress", [dest, amt / 1_000_000, "dryrun"])
        except RuntimeError as e:
            msg = str(e)
            if "have" in msg and "need" in msg:
                import re
                m = re.search(r"have (\d+), need (\d+)", msg)
                have, need = int(m.group(1)), int(m.group(2))
                amt = amt - (need - have) - 500
                continue
            raise
        if not isinstance(d, dict):
            raise RuntimeError(f"dryrun returned {type(d).__name__}: {repr(d)[:120]}")
        if d["change_sats"] == 0:
            return amt, d["fee_sats"]
        amt += d["change_sats"]
    raise RuntimeError("no convergence")

def main():
    log({"event": "manual-seq-start", "tip": rpc("getblockcount"),
         "seed_rows": seed_rows()})
    for dest, tag in SENDS:
        if time.time() > DEADLINE:
            log({"event": "deadline", "tag": tag}); return
        rows = seed_rows()
        if not rows:
            log({"event": "no-seed-coins-left", "tag": tag}); return
        try:
            amt, fee = converge(dest)
        except Exception as e:
            log({"event": "converge-error", "tag": tag, "dest": dest, "err": str(e)[:200]})
            time.sleep(30)
            continue
        before = rpc("getrawmempool")
        if before:
            log({"event": "mempool-busy-wait", "tag": tag, "pool": before}); 
            time.sleep(30)
        try:
            raw = rpc("sendtoaddress", [dest, amt / 1_000_000])
        except Exception as e:
            log({"event": "send-error", "tag": tag, "dest": dest,
                 "amount_sats": amt, "err": str(e)[:220]})
            time.sleep(30)
            continue
        txid = raw["txid"] if isinstance(raw, dict) else None
        log({"event": "sent", "tag": tag, "dest": dest, "amount_sats": amt,
             "fee_sats": fee, "raw_resp_type": type(raw).__name__,
             "raw_resp": repr(raw)[:200], "txid": txid})
        # mine-wait: mempool empties when our single tx mines
        t0 = time.time()
        while time.time() - t0 < 420:
            mem = rpc("getrawmempool", [], 15)
            if not mem:
                break
            time.sleep(10)
        rec = json.load(open("/var/lib/tri7/sent_txs.json"))["entries"]
        last = rec[-1]["txid"] if rec else None
        log({"event": "mined-or-settled", "tag": tag, "mempool": rpc("getrawmempool", [], 15),
             "last_sent_record_txid": last,
             "confirmations": (rpc("getrawtransaction", [last, True]) or {}).get("confirmations") if last else None})
    time.sleep(15)
    log({"event": "manual-seq-done", "seed_rows": seed_rows(),
         "dns2_balance_tri": rpc("getbalance"), "tip": rpc("getblockcount")})

if __name__ == "__main__":
    main()
