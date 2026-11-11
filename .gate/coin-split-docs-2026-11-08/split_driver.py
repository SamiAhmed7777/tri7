#!/usr/bin/env python3
"""COIN-SPLIT item 10, attempt 2 (post 6ed553f fix): drain the 10 shared seed
addresses on DNS2 into two exclusive zones. One tx per source address,
richest first, zero change (dryrun-iterated fee, backing off to the
SELECTABLE pool when fresh rewards are immature), waiting for each tx to
mine. Round 2 sweeps any residue (matured late rewards) to the same zone."""
import json, re, time, sys, urllib.request

RPC = "http://127.0.0.1:19112"
SEED_ADDRS = [
    "TSt5LsVXdc332z1Q2CyQJJ6EZwnJwKwxTx", "TAEM6CbP8Mxusq6Pb1b3zDEBnaQaXD1iJq",
    "TT3sGw6oM89J3XhVQAjfwAvm47jVceAkpN", "TTf2udT4DW64Do3LEnf9RUQoNatiHANnQw",
    "THqUXnZWjbCamggMs8YUsipX4KhXchDPo2", "TUDiACrYTPhYkhp1Yaa8W8gndnkyLWKuBm",
    "TRsiRzkMWm87ZuWFwPB8YXFGYr5AQZo7fb", "TAHtDT9n5UZuD953vqWiXs9eBWFLwwCosg",
    "TBWLnkuXmGBeYbRQxhvXSLrfj3YtFftoGL", "TBzTNFQgN2sKHpZp3h1ev9jnx5S21YnxDM",
]
PC_DESTS = [
    "TA5SHCmj3BQbMcRNcYP5F1RjP7ZWEVCBvs", "TA9DVnUqdUS3WcvV1tZyEAjwmq3M9sRdtH",
    "TABe4ay54yvZVHBSL13gfFVfd35S1U1wmD", "TAGKGAiLTFojNS51xZD2cMG1sVLaRxzJXG",
    "TAJp49gDQMZUM1eTKdYxcMNzK5wL4bP57q",
]
Z2_DESTS = [
    "TVVcEE95oXp9trwdGBRscedKgXeTqCZiJT", "TVkRs3MFRZbaqFu69rxWYuF69Tf64kPRaT",
    "TE5AnHMj9i15KYk6tW4Ztk6FobjwWYkGc8", "TQYsQtEdSCoM5rS1Js1fK9tHnoXtrCJWvZ",
    "TGNJduic9bvTF8Btdz2iBfbk8LdUTXWw4N",
]
LOG = "/root/tri7-lab/split_log.jsonl"
DEADLINE = time.time() + 46 * 60
RESIDUE_FLOOR = 2000  # sats

def rpc(method, params=None, timeout=30):
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

def seed_balances():
    lu = rpc("listunspent", [], 60)
    per = {}
    for row in lu:
        a = row.get("address")
        if a in SEED_ADDRS:
            per[a] = per.get(a, 0) + int(round(row["amount"] * 1_000_000))
    return per

def drain_one(src_bal_sats, dest):
    """Zero-change drain. The dryrun reveals the SELECTED inputs' economics:
    change = selected_total - amt - fee, so the exact zero-change amount is
    amt += change (iterates to convergence). When the ask exceeds the
    SELECTABLE pool (fresh stake rewards still immature -> 'have N, need M'),
    back the amount off by the deficit and retry."""
    amt = src_bal_sats - 2000
    fee = None
    for _ in range(12):
        try:
            d = rpc("sendtoaddress", [dest, amt / 1_000_000, "dryrun"], 60)
        except RuntimeError as e:
            m = re.search(r"have (\d+), need (\d+)", str(e))
            if not m:
                raise
            have, need = int(m.group(1)), int(m.group(2))
            amt = amt - (need - have) - 500
            continue
        fee = d["fee_sats"]
        change = d["change_sats"]
        if change == 0:
            break
        amt += change
    else:
        raise RuntimeError(f"zero-change not converged: change={change} fee={fee}")
    rep = rpc("sendtoaddress", [dest, amt / 1_000_000], 60)
    return rep, amt, fee

def wait_mined(txid, timeout_s=420):
    t0 = time.time()
    while time.time() - t0 < timeout_s:
        try:
            if txid not in rpc("getrawmempool", [], 15):
                return True
        except Exception:
            pass
        time.sleep(10)
    return False

def main():
    log({"event": "start2", "tip": rpc("getblockcount")})
    assigned = {}
    done = 0
    attempts = {}
    while done < 10 and time.time() < DEADLINE:
        per = seed_balances()
        live = {a: b for a, b in per.items() if b > RESIDUE_FLOOR and a not in assigned}
        if not live:
            log({"event": "round1-no-more-addrs", "done": done}); break
        src = max(live, key=live.get)
        bal = live[src]
        zone = "pc" if done % 2 == 0 else "z2"
        dest = (PC_DESTS if zone == "pc" else Z2_DESTS)[done // 2]
        attempts[src] = attempts.get(src, 0) + 1
        try:
            rep, amt, fee = drain_one(bal, dest)
            txid = rep["txid"]
            assigned[src] = {"zone": zone, "dest": dest}
            log({"event": "sent", "round": 1, "src": src, "dest": dest, "zone": zone,
                 "inputs_sats": bal, "amount_sats": amt, "fee_sats": fee,
                 "txid": txid, "attempt": attempts[src]})
            mined = wait_mined(txid)
            log({"event": "mined" if mined else "mine-wait-timeout", "round": 1, "txid": txid,
                 "tip_after": rpc("getblockcount")})
            done += 1
        except Exception as e:
            log({"event": "send-error", "round": 1, "src": src, "dest": dest, "zone": zone,
                 "bal": bal, "attempt": attempts[src], "err": str(e)[:220]})
            if attempts[src] >= 4:
                log({"event": "give-up-on-addr", "src": src})
                time.sleep(30)
            else:
                time.sleep(40)
    # ---- round 2: residue sweep (matured late rewards) to the same zone ----
    for _round in range(3):
        if time.time() > DEADLINE:
            break
        per = seed_balances()
        resid = {a: b for a, b in per.items() if b > RESIDUE_FLOOR}
        if not resid:
            break
        log({"event": "residue-round", "balances": resid})
        for src, bal in sorted(resid.items(), key=lambda kv: -kv[1]):
            asg = assigned.get(src)
            if not asg:
                log({"event": "residue-unassigned-skip", "src": src, "bal": bal})
                continue
            try:
                rep, amt, fee = drain_one(bal, asg["dest"])
                txid = rep["txid"]
                log({"event": "sent", "round": 2, "src": src, "dest": asg["dest"],
                     "zone": asg["zone"], "inputs_sats": bal, "amount_sats": amt,
                     "fee_sats": fee, "txid": txid})
                wait_mined(txid)
                log({"event": "mined", "round": 2, "txid": txid})
            except Exception as e:
                log({"event": "residue-error", "src": src, "err": str(e)[:200]})
                time.sleep(30)
        time.sleep(60)  # let any in-flight stake rewards mature
    per = seed_balances()
    log({"event": "done2", "drained_addrs": len(assigned),
         "assignments": assigned,
         "residual_seed_balances_sats": per,
         "dns2_balance_tri": rpc("getbalance"),
         "tip": rpc("getblockcount")})

if __name__ == "__main__":
    main()
