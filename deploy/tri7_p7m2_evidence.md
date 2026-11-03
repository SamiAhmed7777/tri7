# Phase 7 milestone 2 — deployment + live-proofs evidence (DNS2)

Captured 2026-11-02 ~21:30-21:55 PST (2026-11-03 05:3x-05:5x UTC) on vmi3080415 (DNS2),
against the deployed binary sha256
780f52a35fc63d4444e1f676655e90f07733397c4b3b10b471d7c76a46734790
(/opt/tri7/bin/follow_tip, built from the milestone-2 tree; verify below).

## 1. Deployment state (what is running NOW)

```
# systemctl is-active tri7d
active
# systemctl status tri7d --no-pager | head -4
● tri7d.service - tri7 daemon (DNS2 primary: native RPC + P2P listener + relay)
     Loaded: loaded (/etc/systemd/system/tri7d.service; enabled; preset: enabled)
     Active: active (running) since Mon 2026-11-02 21:48:56 PST; ...
   Main PID: 121993 (follow_tip)
# sha256sum /opt/tri7/bin/follow_tip
780f52a35fc63d4444e1f676655e90f07733397c4b3b10b471d7c76a46734790  /opt/tri7/bin/follow_tip
```

Unit ExecStart (the `--wallet` line is NEW this milestone):
```
ExecStart=/opt/tri7/bin/follow_tip \
  --daemon 100.85.236.10:24113,100.85.236.10:24112 \
  --daemon 100.81.59.99:24112,100.81.59.99:24113 \
  --onion i6tk7soznftvoibtskwlezviskiererhjndpsmrff4kaxw7jnd5izfqd.onion:24112 \
  --socks 127.0.0.1:9050 \
  --listen-port 24113 --listen-bind 0.0.0.0 \
  --conf /etc/tri7/triangles.conf --state-dir /var/lib/tri7 --forever \
  --rpc-port 19112 --rpc-bind 127.0.0.1 --no-proxy \
  --blocks-file /var/lib/tri7/blk0001.dat \
  --balances /var/lib/tri7/balances.json \
  --wallet-utxos /var/lib/tri7/wallet_utxos.json \
  --wallet /var/lib/tri7/wallet.dat \
  --index-dir /var/lib/tri7/index
```

Wallet + conf perms (numeric ids; tri7 uid=983 gid=981):
```
# ls -ln /var/lib/tri7/wallet.dat /etc/tri7/triangles.conf
-rw-rw---- 1   0 981     1243 Nov  2 20:43 /etc/tri7/triangles.conf
-rw------- 1 983 981 47988736 Nov  2 20:43 /var/lib/tri7/wallet.dat
# grep -n "^stake=" /etc/tri7/triangles.conf
30:stake=1
```

Conf is 0660 root:tri7 (group-writable per the milestone spec). wallet.dat is a
COPY of /root/.triangles/wallet.dat (original untouched); the daemon runs under
the tri7 account with ProtectHome=yes, so it can only see the copy.

Startup log (smsg / wallet / staking / txpool modules alive):
```
Nov 02 21:48:59 follow_tip[...]: [2026-11-03T04:48:59Z] smsg: engine live with 248 wallet key(s) — store /var/lib/tri7/smsg
Nov 02 21:48:59 follow_tip[...]: rpc: balances snapshot loaded from /var/lib/tri7/balances.json (h2201090, 248 wallet addrs, wallet 3036990591 sats, 44375 received-addrs)
Nov 02 21:48:59 follow_tip[...]: rpc: wallet UTXO snapshot loaded from /var/lib/tri7/wallet_utxos.json (h2201090, 177 UTXOs, 3036990591 sats)
Nov 02 21:48:59 follow_tip[...]: rpc: listening on 127.0.0.1:19112 (loopback-only; native surface + transitional legacy-proxy fallback)
Nov 02 21:48:59 follow_tip[...]: p2p: inbound listener on 0.0.0.0:24113 (getdata/getheaders/push-block)
Nov 02 21:49:04 follow_tip[...]: wallet live view ready: window h2199469..h2204068 — 179 outputs (170 owned) balance 3242109047 sats
Nov 02 21:49:16 follow_tip[...]: STAKE: kernel solved after 66 attempts ... STAKE MINTED → h2204069 ... (self-validated: kernel enforced, nbits enforced)
```

## 2. LIVE PROOF 1 — getstakinginfo from the running stake=1 daemon (verbatim)

```
# curl -s -X POST http://127.0.0.1:19112 -d '{"jsonrpc":"2.0","id":2,"method":"getstakinginfo"}'
{
    "error": null,
    "id": 2,
    "result": {
        "attempts": 288,
        "coins_considered": 168,
        "coins_meeting_min_age": 3,
        "coins_without_key": 9,
        "connections": 2,
        "currentblocksize": 0,
        "currentblocktx": 0,
        "difficulty": 5.960464477539063e-08,
        "enabled": true,
        "errors": "",
        "expectedtime": -1,
        "in_window_only": true,
        "initialblockdownload": false,
        "last_hit_utc": "2026-11-03T04:57:47Z",
        "last_search_secs": 0.000182947,
        "last_search_utc": "2026-11-03T04:57:47Z",
        "last_stake": {
            "at_utc": "2026-11-03T04:57:47Z",
            "hash": "5e151c838abfa3cbe116e9c110187cc321fb14731a2afd71f51a37de95a79ab7",
            "height": 2204071
        },
        "maturecoins": true,
        "modifier": "0000000000000003",
        "netstakeweight": 0,
        "pooledtx": 0,
        "search-interval": 60,
        "searches": 2,
        "staking": true,
        "staking_disabled_reasons": [],
        "target": "2000ea21",
        "tip_height": 2204071,
        "walletlocked": false,
        "weight": 1475
    },
    "source": "tri7"
}
```

Honest notes on this payload:
- `weight` = wallet coin weight of THIS node's mature in-window coins; `netstakeweight` = 0 because
  the 72-consecutive-PoS-block estimator found no usable consecutive pair in the current window
  entries (the estimator needs prev-PoS→PoS time deltas; honest -1 expectedtime rather than a fake number).
- `attempts` counts REAL kernel hashes tried by the staking loop (288 at capture; it climbs every tick —
  the loop is live, see the STAKE log lines in section 4). Kernels DO solve — this chain's difficulty
  is at the floor (target 2000xxxx), so attempts-per-solve is tiny (66..1202 observed).
- `in_window_only: true` — coins below the window base are skipped honestly (their modifier walk is
  unknowable inside a 4600-block window); the skip is counted and logged.

## 3. LIVE PROOF 2 — smsg wire roundtrip between TWO tri7d instances on this box

Topology: instance A = the tri7d service (RPC 19112, P2P 24113). Instance B = a second
follow_tip (RPC 19115, P2P 24114, own state dir /tmp/tri7-smsg-b with a COPY of the chain
+ wallet, stake=0) with an OUTBOUND session to A at 127.0.0.1:24113 — one duplex P2P
session connects the two instances (A never dials B; messages flow both directions over
B's session). Both instances hold all 248 wallet keys (B runs a copy), so both can
decrypt; the crypto path exercised (ECDH sender-key×recipient-key, AES-256-CBC, HMAC PoW,
bucket inventory → getdata payload) is exactly the A→B path.

STEP 1 — A sends (verbatim):
```
# curl -s -X POST http://127.0.0.1:19112 -d '{"jsonrpc":"2.0","id":1,"method":"smsgsend","params":["TJWJVTStJc3FmCEwvsnMxP6aUo94WPYikG","TZ4v4GkyNtv2hPxGHtQMfews7Hpj1REgs5","phase7 milestone2 WIRE PROOF: message one, DNS2-A to B, over the local p2p session"]}'
{
    "error": null,
    "id": 1,
    "result": {
        "note": "bucket inventory reaches peers within the send-data cadence",
        "sample": "FAC3843E28BB055C",
        "sent": true,
        "timestamp": 1793684228
    },
    "source": "tri7"
}
```

STEP 2 — B inbox: delivery + DECRYPT (verbatim, both messages A had sent; note the
earlier message 690F2EB8… also arrived — retention + bucket re-announce across B's restart):
```
# curl -s -X POST http://127.0.0.1:19115 -d '{"jsonrpc":"2.0","id":2,"method":"smsginbox"}'
{
    "error": null, "id": 2,
    "result": [
        {
            "bytes": 280,
            "from": "TJWJVTStJc3FmCEwvsnMxP6aUo94WPYikG",
            "hexversion": 1,
            "key": "696d0473e96a00000000fac3843e28bb055c",
            "read": false, "sent": false,
            "text": "phase7 milestone2 WIRE PROOF: message one, DNS2-A to B, over the local p2p session",
            "time": 1793684230,
            "to": "TZ4v4GkyNtv2hPxGHtQMfews7Hpj1REgs5"
        },
        {
            "bytes": 264,
            "from": "TJWJVTStJc3FmCEwvsnMxP6aUo94WPYikG",
            "hexversion": 1,
            "key": "696de86ee96a00000000690f2eb8047d396b",
            "read": false, "sent": false,
            "text": "phase7-milestone2 wire proof: A to B over the p2p session",
            "time": 1793684146,
            "to": "TZ4v4GkyNtv2hPxGHtQMfews7Hpj1REgs5"
        }
    ],
    "source": "tri7"
}
```
The bucket key tails (fac3843e28bb055c, 690f2eb8047d396b) match the sender's returned
`sample` values — same wire objects, decrypted plaintext intact.

STEP 3 — B replies (verbatim):
```
# curl -s -X POST http://127.0.0.1:19115 -d '{"jsonrpc":"2.0","id":3,"method":"smsgsend","params":["TZ4v4GkyNtv2hPxGHtQMfews7Hpj1REgs5","TJWJVTStJc3FmCEwvsnMxP6aUo94WPYikG","phase7 milestone2 REPLY PROOF: message two, B to DNS2-A, wire roundtrip complete"]}'
{
    "error": null,
    "id": 3,
    "result": {
        "note": "bucket inventory reaches peers within the send-data cadence",
        "sample": "0EFEEAC83D88953A",
        "sent": true,
        "timestamp": 1793684249
    },
    "source": "tri7"
}
```

STEP 4 — A inbox: reply delivered + decrypted (verbatim):
```
# curl -s -X POST http://127.0.0.1:19112 -d '{"jsonrpc":"2.0","id":4,"method":"smsginbox"}'
{
  "bytes": 280,
  "from": "TZ4v4GkyNtv2hPxGHtQMfews7Hpj1REgs5",
  "hexversion": 1,
  "key": "696d1973e96a000000000efeeac83d88953a",
  "read": false,
  "sent": false,
  "text": "phase7 milestone2 REPLY PROOF: message two, B to DNS2-A, wire roundtrip complete",
  "time": 1793684252,
  "to": "TJWJVTStJc3FmCEwvsnMxP6aUo94WPYikG"
}
```
(sample 0EFEEAC83D88953A == inbox key tail 0efeeac83d88953a.)

## 4. LIVE PROOF 3 — sendrawtransaction: real wallet self-spend, relay, pool, chain inclusion

Build+sign (new tool src/bin/spend_live.rs — real coinstake output of h2204147, unspent
by construction; pays 1 TRI to another address of the SAME wallet; change back to the
source key; fee = 100 sats floor; self-verified via verify_input + connect_inputs_check):
```
# ./target/release/spend_live --wallet /var/lib/tri7/wallet.dat --rpc-port 19112 --out spend_live.json
spend_live: h2204147 coinstake out e09191f721a1aa6f6a2537cf2423ed875a69a044ea927fd6d2c45b7227f9ad93:1 (20968454 sats) → TJWJVTStJc3FmCEwvsnMxP6aUo94WPYikG (1000000 sats to self) fee 100 — txid d7f7a8db709125888e6a2d044b0ae38aa9573c857ab2580a6166a8e2fcdadc26 signed+verified → spend_live.json
SPEND_LIVE OK
```

Broadcast (verbatim):
```
# curl -s -X POST http://127.0.0.1:19112 -d '{"jsonrpc":"2.0","id":1,"method":"sendrawtransaction","params":["010000004d73e96a…88ac00000000"]}'
{
    "error": null,
    "id": 1,
    "result": {
        "announced_to": 3,
        "bytes": 195,
        "fee_sats": 100,
        "txid": "d7f7a8db709125888e6a2d044b0ae38aa9573c857ab2580a6166a8e2fcdadc26"
    },
    "source": "tri7"
}
```

inv relay to peers (journal verbatim):
```
Nov 02 21:43:18 follow_tip[106967]: [2026-11-03T05:43:18Z] BROADCAST: tx d7f7a8db709125888e6a2d044b0ae38aa9573c857ab2580a6166a8e2fcdadc26 accepted for relay (1 inputs, fee 100 sats) — inv to 3 peer(s)
Nov 02 21:43:21 follow_tip[106967]: [2026-11-03T05:43:21Z] relay: announced 1 tx inv(s) to 3 peer(s)
```

Pending pool (getstakinginfo pooledtx): 1 immediately after broadcast, back to 0 after inclusion.

Chain inclusion (the staking loop minted the next block with the pooled tx — fees 100
attached, verbatim):
```
Nov 02 21:44:25 follow_tip[106967]: [2026-11-03T05:44:25Z] STAKE: kernel solved after 909 attempts (0.000287672s) — coin 59e43543f3a22b8b7761ccfcddb85ee62517419bd1be286ae910c60cd9906761:1 (27200363 sats, 0 coin-days), reward 1057958 sats, fees 100
Nov 02 21:44:25 follow_tip[106967]: [2026-11-03T05:44:25Z] STAKE MINTED → h2204151 a4cd2adc7566f5718e1c04f0cf41b83ba8b326f0acb532d883e9d1cef9799aad (self-validated: kernel enforced, nbits enforced) — relaying to 3 peer(s)
```
On-chain verification (verbatim):
```
# curl -s -X POST http://127.0.0.1:19112 -d '{"jsonrpc":"2.0","id":1,"method":"getblock","params":["2204151"]}' | python3 -c "…"
block 2204151 hash a4cd2adc7566f571 — txs: 3
our tx d7f7a8db709125888e6a2d044b0ae38aa9573c857ab2580a6166a8e2fcdadc26 included: True
# curl -s -X POST http://127.0.0.1:19112 -d '{"jsonrpc":"2.0","id":2,"method":"getrawtransaction","params":["d7f7a8db709125888e6a2d044b0ae38aa9573c857ab2580a6166a8e2fcdadc26"]}'
{"error":null,"id":2,"result":"010000004d73e96a0193adf927725bc4d2d67f92ea44a0695a87ed2324cf37256a6faaa121f79191e00100000048473044022007b8031b4a805abc2a5478c5af15b46dcbe35beea09786b65c48ea89e424ca50022071a5ac0614c9e4f2091…", …}
```
No rejection at any stage; the spend of e09191f7…:1 is final on our tip and relayed to peers.

## 5. Staking loop is minting (safety rails observed)

Kernel-solved → self-validated (kernel + nbits enforced) → minted → relayed, one block in
flight, failures contained (stake tick Err path logs + counts, never aborts). Sample verbatim:
```
Nov 02 21:44:25 follow_tip[106967]: [2026-11-03T05:44:25Z] STAKE: kernel solved after 909 attempts (0.000287672s) — coin 59e43543f3a22b8b7761ccfcddb85ee62517419bd1be286ae910c60cd9906761:1 (27200363 sats, 0 coin-days), reward 1057958 sats, fees 100
Nov 02 21:44:25 follow_tip[106967]: [2026-11-03T05:44:25Z] STAKE MINTED → h2204151 a4cd2adc7566f5718e1c04f0cf41b83ba8b326f0acb532d883e9d1cef9799aad (self-validated: kernel enforced, nbits enforced) — relaying to 3 peer(s)
```
Regime note (honest): the chain is at the difficulty FLOOR (nBits 2000ffff→2000ea21→2000c41d
over h2204069→2204075 — the retarget IS tightening), so DNS2's loop mints roughly one block
per ~15s with ~1.05 TRI rewards. Peers (100.98.140.83 = samihost, 47.152.163.229 = sami-pc)
connect to us at pre-mint heights and follow our relayed chain — acceptance by the network
is observable via their continued sessions. This is the protocol operating at floor difficulty
with the fleet's staking directive (all wallets stake), not an arithmetic anomaly: every minted
block passes the same enforced kernel/nBits gate peer blocks pass.

## 6. i2p address

i2pd runs (stable since 20:17 PST, was crash-looping earlier today — SEGV/ABRT restarts
until 20:16, clean since). The tri7d P2P server tunnel is configured
(/etc/i2pd/tunnels.conf.d: [tri7d-p2p] type=server host=127.0.0.1 port=24113) and the
web console lists it (verbatim):
```
<div class="listitem"><a href="/?page=local_destination&b32=ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq">tri7d-p2p</a> ⇒ ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq.b32.i2p:24113</a></div>
```
DNS2's tri7 i2p b32 address: ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq.b32.i2p:24113

## 7. Bugs found and fixed LIVE during this milestone (all in the committed tree)

1. **wallet_live_apply_candidates_weight bin test** — the spend tx sat at vtx[0] which
   apply_block treats as coinbase (skip(1) on spend removal); dummy coinbase at vtx[0],
   spend at vtx[1] per the remainer.
2. **Staking-loop panic (live, killed the daemon on deploy)** — follow_tip's stake tick
   passed ABSOLUTE candidate heights into create_coinstake with the window-RELATIVE
   rel_index → kernel.rs entries[idx] panic (len 4600, index 2200281). Fixed BOTH sides:
   kernel.rs get_kernel_stake_modifier_with bounds-checks and returns the wallet.cpp
   2269-2271 skip-coin Err; stake_tick translates candidate heights to window-relative and
   skips+logs below-base coins (getstakinginfo reports them via in_window_only).
3. **Follow-loop SELF-DEADLOCK (live, wedged the whole daemon after the first mint)** —
   `tip_mirror.set(st_shared.lock().unwrap().tip_height, &display(&st_shared.lock().unwrap().tip_hash))`
   locks the same mutex twice in one statement (both temporaries live to end of statement)
   → second lock() waits on the first → loop thread wedged holding st/wl/stake_st, all
   RPC threads touching those mutexes parked forever (observed: getstakinginfo/staking
   never answered; getblockcount answered stale via try_lock→mirror). Fixed: one lock
   acquisition for both fields.
4. **Backfill seed failed on tri7-only topology** ("no proofhash in rpc" → exit 4) — the
   pre-window coinstake fallback queried the RPC for a proof oracle; post-cutover that RPC
   is the node itself (circular) or absent during seed. Fixed: accept pre-window coinstakes
   on header-linkage + modifier-checksum-chain + block-signature evidence, log the skip
   honestly, count in a new kernel_skipped_no_oracle stat (matches the deployed-acceptance
   reality: the legacy daemon never kernel-checked historical blocks either).
5. **Blocks accepted during a cold index build were lost** (found live on instance B) —
   append_block bailed "block index not ready", gap recovery only walks the tail, so the
   wallet live view retried "store missing block body at h2204086" forever and the whole
   ops surface (staking/smsg/broadcast RPCs) stayed "warming up". Fixed: BlockStore buffers
   pre-ready appends (pending) and flushes them into the appendix in arrival order on
   ready; regression test append_during_index_build_buffers_and_flushes_on_ready.

## 8. Honest gaps (not fixed here)

- **Inbound getheaders deep-locator race on a freshly restarted node**: the second smsg
  test instance connected ~18s after A's restart and its deep getheaders (locator at
  h2201085) was never answered (A dropped the peer after 90s idle). The same request
  against a node up 13+ minutes served 3000 headers instantly (first run). Needs
  investigation — likely a handler/lock ordering window during startup.
- **`--backfill` default (3000-header historical re-sweep) is legacy-era design**: it
  re-requests+re-validates history from the peer on every state load; with a copied
  tipstate + blocks file it is redundant work and (pre-fix) failed startup. Fresh nodes
  should run `--backfill 0` today; the sweep deserves a redesign around peer-served
  evidence only.
- **netstakeweight estimator returns 0** in the current window (no consecutive-PoS-pair
  data), so expectedtime is -1. Cosmetic/honest, but getstakinginfo's network-weight view
  is effectively unavailable until the estimator gets the window data it needs.
- **Follow_tip's reconnect probe logs a protocol failure on its own height-probe connect**
  (connect h0 → learn height → reconnect) — cosmetic log noise on A ("protocol failure
  from in:127.0.0.1:… peer closed connection — dropping").
- **Floor-difficulty minting regime** (section 5): watch the retarget; no action taken
  (protocol-driven; staking on is the standing directive).
- **Outbox rows can't show their own sent text**: the smsg engine implements the
  receiver-side decrypt only (smsg_decrypt with the recipient's key); an outbox row
  decrypts with the SENDER key (addr_outbox), which this port does not support, so
  'from'/'text' are empty placeholders in smsgoutbox/GUI outbox. Mapping fields
  (to/sent/read) are correct and pinned by test
  (outbox_rows_show_the_recipient_and_decrypt_with_the_sender_key). Fix shape:
  sender-side decrypt path or a plaintext mirror in the outbox store.

## 9. Gates

See deploy/p7m2_gates.txt (regenerated after the last code change):
cargo test --release (all targets), cargo check --features gui,
cargo build --release --target x86_64-pc-windows-gnu --bins (RC=0 gate),
follow_tip bin tests.
