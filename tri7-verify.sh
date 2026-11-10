#!/usr/bin/env bash
# tri7-verify.sh — reproducible verification of the claims in TRI7-COORDINATION.md.
# Run on the host that runs tri7d (atis: DNS2). Filesystem writes: ONLY the mktemp
# files it creates and removes; requires git, curl, journalctl, python3, GNU stat.
# Note: wallet/prevout addresses are taken from the node's own scriptPubKey.addresses
# annotation and matched against the decoded wallet set (RPC-reported verification,
# not an independent re-decode of raw scripts).
set -euo pipefail
cd /root/tri7

echo "host=$(hostname)  when=$(date -Is)"
echo "repo_head=$(git rev-parse HEAD)"
echo "repo_head_short=$(git rev-parse --short HEAD)"

echo; echo "== implementation commit refs =="
for f in src/txpool.rs src/rpc.rs src/stake.rs src/spend.rs src/node_embedded.rs src/bin/tri7_wallet_gui.rs; do
  printf '%-28s %s\n' "$f" "$(git log -1 --format='%h %ad %s' --date=format:'%Y-%m-%d' -- "$f")"
done

echo; echo "== sendrawtransaction: advertised + implemented =="
grep -n 'sendrawtransaction' src/rpc.rs | sed -n '1,3p'
grep -n '"sendrawtransaction" =>' src/node_embedded.rs | head -2
test -s src/txpool.rs && echo "txpool.rs present: $(wc -l < src/txpool.rs) lines"

echo; echo "== mint includes pool txs =="
grep -n 'txs_for_block' src/node_embedded.rs | head -2

echo; echo "== follower validator: structure-only scan for extra txs =="
grep -n 'for tx in block.vtx.iter().skip(1)' src/node_embedded.rs | head -3

echo; echo "== live mint cadence (last 60 min) =="
N=$(journalctl -u tri7d --since "-60 min" --no-pager | grep -c 'STAKE MINTED' || true)
echo "STAKE MINTED lines: $N (varies with stake luck; expect tens per hour)"
journalctl -u tri7d --since "-60 min" --no-pager | grep 'STAKE MINTED' | tail -2 || true

echo; echo "== live getinfo =="
curl -sS --connect-timeout 5 --max-time 20 -X POST http://127.0.0.1:19112 \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getinfo"}' | python3 -c '
import json,sys
r=json.load(sys.stdin)["result"]
print("balance",r["balance"],"coverage_h",r["balance_coverage_height"],"blocks",r["blocks"],"connections",r["connections"])'

echo; echo "== 300-block tx-count scan (exact bounds recorded) =="
python3 - <<'PY'
import json, urllib.request
from datetime import datetime, timezone
def rpc(m, p=None):
    d={"jsonrpc":"2.0","id":1,"method":m}
    if p: d["params"]=p
    req=urllib.request.Request("http://127.0.0.1:19112",data=json.dumps(d).encode(),headers={"Content-Type":"application/json"})
    return json.load(urllib.request.urlopen(req,timeout=20)).get("result")
t0=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
tip=rpc("getblockcount")
lo=max(tip-299,1)
multi=[]; checked=0
for h in range(tip, lo-1, -1):
    b=rpc("getblock",[rpc("getblockhash",[h])]); checked+=1
    if len(b.get("tx",[]))>2: multi.append((h,len(b["tx"])))
t1=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
print(f"scan_start={t0} scan_end={t1}")
print(f"tip_at_scan={tip} scanned={checked} range={lo}..{tip}")
print(f"blocks_with_more_than_2_txs={len(multi)}")
for h,n in multi[:5]: print("  multi-tx block:",h,"txs:",n)
PY

echo; echo "== relay pool bounds (code facts) =="
grep -n "DEFAULT_POOL_CAP\|DEFAULT_ENTRY_TTL_SECS" src/txpool.rs | head -3

echo; echo "== stored signed spend: broadcast:false in the JSON is STALE =="
python3 - <<'PY'
import json
d=json.load(open("/root/tri7/spend_live.json"))
print("mode",d["mode"],"| json says broadcast",d["broadcast"],"| amount_sats",d["amount_sats"],
      "| txid",d.get("txid","?")[:20],"| signed_at_block",d["source_block"])
PY

echo "== verbose chain lookup of that txid (authoritative; missing txid FAILS) =="
TXID=$(python3 -c 'import json;print(json.load(open("/root/tri7/spend_live.json")).get("txid","") or "")')
[ -n "$TXID" ] || { echo "VERIFY FAIL: spend_live.json has no txid"; exit 1; }
V=$(mktemp)
trap 'rm -f "$V"' EXIT
curl -sS --connect-timeout 5 --max-time 20 -X POST http://127.0.0.1:19112 \
  -H 'Content-Type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getrawtransaction\",\"params\":[\"$TXID\",true]}" \
  -o "$V"
python3 - "$TXID" "$V" <<'PY'
import json,sys
txid,path=sys.argv[1],sys.argv[2]
r=json.load(open(path)).get("result")
if not isinstance(r,dict):
    print("VERIFY FAIL: tx not in chain (result:",repr(r)[:80],")"); raise SystemExit(1)
vin=r.get("vin",[]); vout=r.get("vout",[])
print("IN CHAIN: txid",r.get("txid",txid))
print("  height",r["height"],"| confirmations",r["confirmations"],"(grow over time)")
print("  blockhash",r["blockhash"])
print("  vin:",[(v.get("txid"),v.get("vout")) for v in vin])
print("  vout values:",[v.get("value") for v in vout])
assert r["height"]>=2204151, r["height"]
assert r["confirmations"]>=1000, r["confirmations"]
assert len(vin)==1 and vin[0].get("txid")=="e09191f721a1aa6f6a2537cf2423ed875a69a044ea927fd6d2c45b7227f9ad93" and vin[0].get("vout")==1, vin
assert len(vout)==2 and vout[0]["value"]==1.0, vout
print("LIVE-SPEND-PROVEN: the stored self-spend (1 TRI out, single input e09191f7…:1) is mined and confirmed")
PY

echo "== wallet ownership DERIVED from the tx scripts (base58-decode, no hardcoding) =="
S=$(mktemp); cp "$V" "$S"   # keep the verbose spend tx before later curls reuse $V
W=$(mktemp); I=$(mktemp); S2=$(mktemp); trap 'rm -f "$V" "$W" "$I" "$S2" "$S"' EXIT
curl -sS --connect-timeout 5 --max-time 20 -X POST http://127.0.0.1:19112 \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"listwalletaddresses"}' -o "$W"
IN_TX=e09191f721a1aa6f6a2537cf2423ed875a69a044ea927fd6d2c45b7227f9ad93
curl -sS --connect-timeout 5 --max-time 20 -X POST http://127.0.0.1:19112 \
  -H 'Content-Type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"getrawtransaction\",\"params\":[\"$IN_TX\",true]}" -o "$I"
python3 - "$W" "$I" "$S" <<'PY'
import json,sys,hashlib
W,I,S=sys.argv[1:4]
ALPH="123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58decode(s):
    n=0
    for c in s: n=n*58+ALPH.index(c)
    raw=n.to_bytes((n.bit_length()+7)//8 or 1,"big")
    body=raw[:-4]; chk=raw[-4:]
    assert hashlib.sha256(hashlib.sha256(body).digest()).digest()[:4]==chk, "bad checksum"
    return body  # version(1) + hash160(20)
wallet=set()
for line in open(W):
    line=line.strip()
    if not line: continue
    try: a=json.loads(line).get("result")
    except Exception: continue
    if isinstance(a,list):
        for x in a:
            if isinstance(x,str): wallet.add(x)
            elif isinstance(x,dict) and x.get("address"): wallet.add(x["address"])
    elif isinstance(a,dict):
        for x in a.get("addresses",[]) or []:
            wallet.add(x if isinstance(x,str) else x["address"])
h2addr={}
for a in wallet:
    try:
        body=b58decode(a)
        if len(body)==21: h2addr[body[1:].hex()]=a
    except Exception: pass
assert len(h2addr)>=200, f"decoded too few wallet addresses ({len(h2addr)})"
def owner(vout):
    # the node reports the addresses for standard scripts (P2PK/P2PKH)
    a=vout["scriptPubKey"].get("addresses") or []
    return a[0] if len(a)==1 else None
# input owner: the prevout's vout[1]
inp=json.load(open(I)).get("result")
o1=owner(inp["vout"][1])
assert o1 and o1 in h2addr.values(), ("input prevout address NOT wallet-owned",o1)
print("ok: input e09191f7…:1 pays to",o1,"(wallet)")
# output owners: the spend tx's two vouts
sp=json.load(open(S)).get("result")
outs=[]
for v in sp["vout"]:
    o=owner(v); assert o and o in h2addr.values(), ("output address NOT wallet-owned",v["scriptPubKey"])
    outs.append(o)
print("ok: spend outputs pay to",outs,"(both wallet)")
import json as _j
want=_j.load(open("/root/tri7/spend_live.json"))["to_address"]
assert outs[0]==want, ("1-TRI output does not match spend_live.json to_address",outs[0],want)
print("ok: vout[0] matches spend_live.json to_address",want)
PY
curl -sS --connect-timeout 5 --max-time 20 -X POST http://127.0.0.1:19112 \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"listunspent"}' -o "$V"
python3 - "$V" <<'PY'
import json,sys
u=json.load(open(sys.argv[1])).get("result") or []
spent=[x for x in u if x.get("txid","").startswith("e09191f7")]
assert not spent, ("input still unspent?!",spent)
print(f"ok: consumed input e09191f7…:1 no longer in listunspent ({len(u)} current UTXOs)")
PY

echo "== relay-pool counter (node's own periodic report; scope not documented upstream — treat as indicative; the 300-block scan is the mined-window evidence) =="
R=$(journalctl -u tri7d --since "-24 hours" --no-pager | grep '"txs_accepted"' | tail -1)
[ -n "$R" ] || { echo "VERIFY FAIL: no txs_accepted report found in the journal"; exit 1; }
echo "$R"
echo "== tri7 services =="
systemctl is-active tri7d tri7d-health.timer
echo "VERIFY-SCRIPT-COMPLETE"
