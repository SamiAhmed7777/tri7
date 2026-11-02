#!/usr/bin/env python3
"""Phase 1e cross-check: tri7 balances.json vs the live daemon RPC (read-only).

1. Supply: daemon getinfo moneysupply is nMoneySupply at the DAEMON tip; the
   archive replay stops at height H (2201090). Bridge exactly:
     daemon_supply(H) = moneysupply(tip) - sum(nMint(h), h=H+1..tip)
   with per-block mint from getblockbynumber (rpcblockchain.cpp blockToJSON).
   tri7's total live-UTXO balance must equal daemon_supply(H) sat-for-sat
   (the daemon's FastImport maintains the UTXO set and the running supply in
   the same walk, so at equal heights they are the same number).
2. Wallet received: for every wallet address,
     getreceivedbyaddress <addr> 0  (rpcwallet.cpp:497: skips coinbase AND
     coinstake txs, matches the P2PKH script bytes exactly, IsMine gate)
   must equal tri7's received_regular_sats for that address.

Usage: rpc_crosscheck.py [balances.json] [rpc_crosscheck.json]
Exit 0 iff supply gap == 0 and all wallet addresses match.
"""

import http.client
import json
import base64
import sys
from pathlib import Path

CONF = "/root/.triangles/triangles.conf"
COIN = 1_000_000


def conf():
    user = pw = port = None
    for line in Path(CONF).read_text().splitlines():
        line = line.strip()
        if line.startswith("rpcuser="):
            user = line.split("=", 1)[1]
        elif line.startswith("rpcpassword="):
            pw = line.split("=", 1)[1]
        elif line.startswith("rpcport="):
            port = int(line.split("=", 1)[1])
    assert user and pw and port, "rpc credentials not found in conf"
    return user, pw, port


class Rpc:
    def __init__(self, user, pw, port):
        auth = base64.b64encode(f"{user}:{pw}".encode()).decode()
        self.hdr = {"Authorization": "Basic " + auth, "Content-Type": "application/json"}
        self.port = port
        self.conn = http.client.HTTPConnection("127.0.0.1", port)
        self.n = 0

    def call(self, method, *params):
        body = json.dumps({"method": method, "params": list(params), "id": 0})
        data = b""
        for attempt in (1, 2, 3):
            try:
                self.conn.request("POST", "/", body, self.hdr)
                resp = self.conn.getresponse()
                data = resp.read()
                break
            except (http.client.HTTPException, OSError):
                if attempt == 3:
                    raise
                self.conn.close()
                self.conn = http.client.HTTPConnection("127.0.0.1", self.port)
        self.n += 1
        obj = json.loads(data)
        if obj.get("error") is not None:
            raise RuntimeError(f"{method}: {obj['error']}")
        return obj["result"]

    def to_sats(self, tri_float):
        return round(tri_float * COIN)


def main():
    balances_path = sys.argv[1] if len(sys.argv) > 1 else "/root/tri7/balances.json"
    out_path = sys.argv[2] if len(sys.argv) > 2 else "/root/tri7/rpc_crosscheck.json"
    bal = json.loads(Path(balances_path).read_text())
    archive_h = bal["height"]
    tri7_total = bal["totals"]["total_balance_sats"]
    tri7_live_utxos = bal["totals"]["live_utxos"]

    user, pw, port = conf()
    rpc = Rpc(user, pw, port)

    info = rpc.call("getinfo")
    tip = info["blocks"]
    supply_sats = rpc.to_sats(info["moneysupply"])
    print(f"daemon tip {tip}, moneysupply {info['moneysupply']} TRI ({supply_sats} sats)")
    print(f"archive tip {archive_h}: tri7 total balance {tri7_total} sats in {tri7_live_utxos} UTXOs")

    # bridge the supply from the daemon tip back to the archive tip
    mint_sats = 0
    for h in range(archive_h + 1, tip + 1):
        b = rpc.call("getblockbynumber", h)
        mint_sats += rpc.to_sats(b["mint"])
    daemon_supply_at_archive = supply_sats - mint_sats
    gap = tri7_total - daemon_supply_at_archive
    print(
        f"bridge: {tip - archive_h} daemon-only blocks mint {mint_sats} sats -> "
        f"daemon moneysupply@{archive_h} = {daemon_supply_at_archive} sats"
    )
    print(f"SUPPLY GAP (tri7 - daemon): {gap} sats")

    # per-address received cross-check
    wallet = bal["wallet"]
    matches = 0
    mismatches = []
    for row in wallet:
        addr = row["address"]
        d = rpc.call("getreceivedbyaddress", addr, 0)
        daemon_sats = rpc.to_sats(d)
        tri7_sats = row["received_regular_sats"]
        if daemon_sats == tri7_sats:
            matches += 1
        else:
            mismatches.append(
                {"address": addr, "daemon_sats": daemon_sats, "tri7_sats": tri7_sats}
            )
    print(
        f"getreceivedbyaddress over {len(wallet)} wallet addresses: "
        f"{matches} match, {len(mismatches)} mismatch"
    )
    for m in mismatches[:20]:
        print(f"  MISMATCH {m['address']}: daemon {m['daemon_sats']} vs tri7 {m['tri7_sats']}")

    result = {
        "daemon_tip": tip,
        "daemon_moneysupply_sats": supply_sats,
        "daemon_only_blocks": tip - archive_h,
        "daemon_only_blocks_mint_sats": mint_sats,
        "daemon_moneysupply_at_archive_sats": daemon_supply_at_archive,
        "tri7_total_balance_sats": tri7_total,
        "supply_gap_sats": gap,
        "wallet_addresses": len(wallet),
        "received_matches": matches,
        "received_mismatches": len(mismatches),
        "mismatch_detail": mismatches,
        "rpc_calls": rpc.n,
    }
    Path(out_path).write_text(json.dumps(result, indent=1) + "\n")
    print(f"written: {out_path}")
    if gap != 0 or mismatches:
        print("CROSSCHECK: MISMATCH")
        sys.exit(1)
    print("CROSSCHECK: ALL MATCH")


if __name__ == "__main__":
    main()
