#!/usr/bin/env python3
"""Checks AFTER the item 53 repair was applied. Read-only. Prints counts only.

  V2  the node's listunspent against the index: every listed outpoint is an
      unspent row in the index, and per address the index holds exactly as
      many unspent rows as the node lists
  V3  every coin the independent walk found canonically spent carries that
      canonical height as its spent_height in the index

Exit 0 only if every check holds. Addresses are never printed.
"""
import base64
import collections
import http.client
import json
import re
import sqlite3
import sys

IDX = "file:/var/lib/tri7/addressindex.db?mode=ro"
CANON = "/root/.hermes/cache/scratch/item53_canonical_spend.json"

conf = open("/etc/tri7/triangles.conf").read()


def key(name):
    m = re.search(r"^" + name + r"=(\S+)", conf, re.M)
    return m.group(1) if m else None


AUTH = "Basic " + base64.b64encode((key("rpcuser") + ":" + key("rpcpassword")).encode()).decode()
PORT = int(key("rpcport") or 19112)


def rpc(method, params):
    conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    conn.request("POST", "/", json.dumps({"jsonrpc": "1.0", "id": 1, "method": method, "params": params}),
                 {"Authorization": AUTH, "Content-Type": "application/json"})
    reply = json.loads(conn.getresponse().read())
    conn.close()
    if reply.get("error"):
        raise SystemExit("RPC {} returned an error; stopped".format(method))
    return reply["result"]


ok = True


def check(cond, what):
    global ok
    print("{}  {}".format("PASS" if cond else "FAIL", what))
    ok = ok and bool(cond)


db = sqlite3.connect(IDX, uri=True, timeout=10)
canon = json.load(open(CANON))["canonical_spend"]

# which byte order does the index store a txid in? decided on one coin, not assumed
sample_key = next(iter(canon))
sample_txid = sample_key.split(":")[0]
blob = None
for cand in (bytes.fromhex(sample_txid), bytes.fromhex(sample_txid)[::-1]):
    n = db.execute("SELECT count(*) FROM out WHERE txid = ? AND n = ?", (cand, int(sample_key.split(":")[1]))).fetchone()[0]
    if n:
        blob = cand
        break
if blob is None:
    raise SystemExit("NEITHER byte order finds the first coin in the index; nothing concluded")
reverse = blob == bytes.fromhex(sample_txid)[::-1]


def as_blob(txid):
    b = bytes.fromhex(txid)
    return b[::-1] if reverse else b


# ---- V3
equal = differ = missing = 0
diff_examples = 0
for key_, height in canon.items():
    txid, n = key_.split(":")
    rows = db.execute("SELECT spent_height FROM out WHERE txid = ? AND n = ?", (as_blob(txid), int(n))).fetchall()
    if not rows:
        missing += 1
        continue
    spent = rows[0][0]
    if spent == height:
        equal += 1
    else:
        differ += 1
        diff_examples += 1
print("V3 coins the independent walk found canonically spent: {} (txid stored {} in the index)".format(
    len(canon), "byte-reversed" if reverse else "as displayed"))
print("    index spent_height == canonical height: {}   differs: {}   no row: {}".format(equal, differ, missing))
check(equal == len(canon), "V3 every one of the {} coins carries its canonical spend height".format(len(canon)))

# ---- V2
unspent = rpc("listunspent", [1, 99999999])
print("V2 node listunspent rows: {}  sum {:.6f}".format(len(unspent), sum(float(x["amount"]) for x in unspent)))
listed_unspent_in_index = 0
per_addr_listed = collections.Counter()
for x in unspent:
    per_addr_listed[x["address"]] += 1
    row = db.execute("SELECT spent_height FROM out WHERE txid = ? AND n = ?", (as_blob(x["txid"]), int(x["vout"]))).fetchone()
    if row and row[0] is None:
        listed_unspent_in_index += 1
check(listed_unspent_in_index == len(unspent),
      "V2 every listed outpoint is an unspent row in the index ({}/{})".format(listed_unspent_in_index, len(unspent)))
index_rows_total = 0
addr_mismatch = 0
for addr, listed in per_addr_listed.items():
    in_index = db.execute("SELECT count(*) FROM out WHERE addr = ? AND spent_height IS NULL", (addr,)).fetchone()[0]
    index_rows_total += in_index
    if in_index != listed:
        addr_mismatch += 1
check(addr_mismatch == 0 and index_rows_total == len(unspent),
      "V2 per address the index holds exactly the listed unspent rows ({} rows over {} addresses; addresses differing: {})".format(
          index_rows_total, len(per_addr_listed), addr_mismatch))
print("RESULT: " + ("ALL CHECKS PASS" if ok else "A CHECK FAILED"))
sys.exit(0 if ok else 1)
