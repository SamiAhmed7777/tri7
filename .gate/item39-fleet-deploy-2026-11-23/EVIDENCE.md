# EVIDENCE — item-39 fleet deploy bundle

Generated 2026-11-23 ~03:0xZ on DNS2. Every live claim below is a fresh command readback;
the protocol results are MEASURED by executing the submitted scripts against stubbed
systemctl/cargo/RPC with real bash/git/install/sha256sum/timeout/python3.

## Submitted artifact digests (bind this evidence to exactly these bytes)
<!-- verbatim `sha256sum` output from 2026-11-23T18:36PST packaging run; never hand-typed -->
```
89081c8956e9efa76bc1fbc8ade7d988f10886a9054d30b12def60cec1f7ec77  BRIEF.md
ed4de177e16711fb153a386765939f951f40b4461c6cc05ced8e3032f2373285  deploy_dns3.sh
9a7ffd7e2c6720e051aae8a8750beb1a66e4bf74e223a1a720ad8791360988b1  deploy_pi.sh
d25d2ad36d03aa2896e6b03c8da1650e8a9e8385bb23959fe5cc67a82165375f  test_harness.py
2ccd8adeab9a61613967c8300587effb00bbc6c26cb8de4c321747e1911df17d  test_RESULTS.md
4de275f4e720fedb714f00e66ccedb57a1284f3f8db8e1cd9ae36e126e14483d  test_transcripts.log
f7127e9b9e15fec09ef76f516803d39554a1b3852528a5f11c7b4810a32ea8e0  test_stub_systemctl.sh
9c8e83d8fd7f8597b0f7588892df73ee8a84e6899b48add4d4cd0ae511dbb174  test_stub_cargo.sh
```

## Artifact identity + provenance (DNS2, already deployed and healthy — these are the bytes the fleet gets)
```
87755353c853b96cfd01d5838d0c66f8f2cc5dc6993209b15287a86c9ea50c5e  /root/tri7/target/release/follow_tip
87755353c853b96cfd01d5838d0c66f8f2cc5dc6993209b15287a86c9ea50c5e  /opt/tri7/bin/follow_tip
87755353c853b96cfd01d5838d0c66f8f2cc5dc6993209b15287a86c9ea50c5e  /proc/2122981/exe
574723943b7f39ef3276e9a57beaf4acb5d99258
574723943b7f39ef3276e9a57beaf4acb5d99258 [grade=A urn:ump:yw4isszergrygqghhosnh2rzim7up5anpxs2sjkn24cqrgxg46aq] wallet: 
```

## CI for the pinned commit 5747239
```
in_progress		[grade=B urn:glm:tri7-selfserve-port-20261123] node: self-serve clien…	CI	master	push	45096896135	20m43s	2026-11-23T01:51:21Z
completed	success	[grade=A urn:glm:tri7-archive-resync-20261122] archive walker debris …	CI	master	push	45091175926	1h18m21s	2026-11-23T00:51:11Z
completed	success	[grade=A urn:ump:yw4isszergrygqghhosnh2rzim7up5anpxs2sjkn24cqrgxg46aq…	CI	master	push	45085582662	1h16m55s	2026-11-22T23:52:33Z
```

## DNS2 live battery (post-deploy 01:35Z — proof the shipped bytes work)
```
tip: 2217821 2ab1b04ec5a3275e2464ed0c328451a8946a5f8f0045272cbf6702b5220e36da time: 1795397701
staking.staking = True
staking.attempts = 1224303
staking.coins_meeting_min_age = 15
staking.staking_disabled_reasons = ['wallet live view holds 1 coin(s) the chain contradicts (e.g. 4f43cd16188b916d5d2c1364bda433758f43c0cbd5e44f4a7be1ae0fec4347a0:1 claims block h2217710 time 1795382768 vs chain 1795383968) — excluded from staking; run repairwallet']
plainbal: 5372.984539 balance_sats: 5372984539
balance_source: live-index partial: False
listunspent rows: 364 total: 5372.984539
TRsxnwXe rows: 6 total: 100.046768 confs: [28, 28, 45, 57, 57, 83]
getaddressbalance TRsxnwXe: {'balance': 100046768, 'received': 450115343, 'index_complete': True}
```

## DNS2 journal: post-boot AUTOHEAL full-history rebuild (the fix, live)
```
Nov 22 16:10:26 vmi3080415 follow_tip[2122981]: [2026-11-23T00:10:26Z] stake: AUTOHEAL — rebuild h0..h2217779 found ZERO owned-output mismatch(es) while the stale flag still fires: RESOLVER FAULT (item 37), NOT wallet corruption — the repairwallet remedy is a no-op for this class; this episode's attempt budget is retired, look at the resolver's chain-evidence lookup, not the wallet
```

## Target nodes pre-deploy state (readback over ssh)
```
DNS3:
702b0517eaf131fd3c17be75aed382d286959e09ef545dff9625f51e5f4df395  /opt/tri7/bin/follow_tip
active
913696
702b0517eaf131fd3c17be75aed382d286959e09ef545dff9625f51e5f4df395  /proc/913696/exe
Pi:
f027bd5dd59d4608493d404f83f6613cefaa50098cf9dc5f2fd1b81b528f23db  /root/tri7/target/release/follow_tip
active
e8f218dcd12683c5eaa873009f1e10f816e3c9df
(git status --porcelain printed nothing above = clean working tree, no .gitmodules)
Pi running inode: f027bd5dd59d4608493d404f83f6613cefaa50098cf9dc5f2fd1b81b528f23db  /proc/2121305/exe (== path digest, detached HEAD, no .gitmodules)
```

## Deterministic protocol test (MEASURED exits of the submitted scripts)
Executed by the submitted self-contained test_harness.py (scripts+stubs resolved
relative to its own directory; output under ./protocol-test-run/). 18 scenarios.
Verbatim header of the submitted test_RESULTS.md:
```
# item39-fleet-deploy — deterministic protocol test results

Run 2026-11-23T02:53:57Z. systemctl/cargo/RPC stubbed; bash/git/install/sha256sum/timeout/python3 real.
Harness resolves scripts and stubs relative to its own directory; all output under ./protocol-test-run/. transcripts.log holds per-scenario stdout.

...result-table rows (incl. header): 75
**73/73 checks passed.**
```
Full per-scenario table: test_RESULTS.md (73 checks, all PASS). Per-scenario stdout:
test_transcripts.log. Harness: test_harness.py. Stubs: test_stub_systemctl.sh,
test_stub_cargo.sh (submitted; bound by the digest block below).
