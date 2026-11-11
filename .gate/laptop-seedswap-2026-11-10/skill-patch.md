# Skill patch — tri7-fleet-chain-state (target: /root/.hermes/skills/tri7-fleet-chain-state/SKILL.md; see skill-anchor.md for the unique anchor + file region)

TWO OPERATIONS, applied via skill_manage in one batch:

## OPERATION 1 — SKILL.md insertion (skill_manage patch)
old_string (verbatim, spans one newline; unique in file — grep count 1):
**Never launch a Windows daemon inline over SSH, and never hand-write its
scheduled task from an SSH command line.**

new_string = the BLOCK below, then a blank line, then the old_string text unchanged:

**Laptop h0 wedge + seed swap (2026-11-10, live-proven):** a fresh/welded GUI
install with no chain snapshot can wedge at height 0 FOREVER: `store_usable`
(src/node_embedded.rs:9470 — the moved follow_tip logic) only checks the
blocks file is NON-EMPTY, so the daemon's own 208-byte genesis frame passes
and `have_state` stays true — the from-zero body fetch never fires
(signature: getsyncinfo `height 0, phase follow, syncing false`, tipstate h0
with stale last_advance; the in-code comment declares the bare-machine intent
the `> 0` check defeats). Cure = full state-trio swap from a canonical node:
stop task `tri7gui-laptop` + taskkill the GUI (the welded build's node is
IN-PROCESS — the GUI process itself answers RPC 19114, no separate
follow_tip.exe exists), rename aside ONLY node\state\blk0001.dat,
tipstate.json, index\, gap_recovery.json (keep peers.json + paytxfee.json +
smsg\), push the trio in, relaunch the task, then verify RPC readiness and tip non-regression (two
observations) and shared-height hash parity vs DNS2. RUN THE
FULL RECIPE FROM THE SCRIPT —
`references/laptop-seed-swap.sh` (fail-fast preflight BEFORE any rename;
process-death verified before renames; each rename independently checked;
source sha256 recorded first; byte-identity readback compare; bounded RPC
readiness poll; computed shared height). Proven: SHA256-identical readback
on all 4 files, parity at h2209120 within ~3 min of relaunch, then the
laptop STAKE MINTED on its own zone coin. Onion dials fail on the laptop
(no tor socks) — the Pi clearnet 443 peer is its lifeline; expected, not a
fault. Fix owed in repo: treat a genesis-frame-only store (tipstate h0, no
bodies above genesis) as bare so from-zero resumes — the `len() > 0` check
at src/node_embedded.rs:9470 is the whole gap.

## OPERATION 2 — supporting script (skill_manage write_file)
file_path: references/laptop-seed-swap.sh
content:

#!/usr/bin/env bash
# Laptop seed swap — full proven recipe (2026-11-10). Run on DNS2 as root.
# Seeds C:\Coins\TRI-LAPTOP\node\state\ from a canonical tri7 state trio.
# Usage: laptop-seed-swap.sh <seed_dir>
#   <seed_dir> holds blk0001.dat, tipstate.json, index/blockidx.bin, index/txidx.bin
# Proof run: h2209120 identical on laptop and DNS2 (3907c385…70cf); laptop
# then STAKE MINTED h2209134 d0e3e776… on its own zone coin.
set -uo pipefail
SEED="${1:?usage: laptop-seed-swap.sh <seed_dir>}"
LAP=sami-laptop
DEST='C:/Coins/TRI-LAPTOP/node/state'
SD='C:\Coins\TRI-LAPTOP\node\state'
die(){ echo "FAIL: $*"; exit 1; }

# ---- 0. PREFLIGHT — fail fast BEFORE touching the laptop's state ----
for f in blk0001.dat tipstate.json index/blockidx.bin index/txidx.bin; do
  [ -s "$SEED/$f" ] || die "missing/empty $SEED/$f"
done
SRC_HASHES=$(cd "$SEED" && sha256sum blk0001.dat tipstate.json index/blockidx.bin index/txidx.bin) \
  || die "cannot hash the seed source"
echo "-- source hashes (authority for the readback compare) --"
echo "$SRC_HASHES"
ssh -o BatchMode=yes -o ConnectTimeout=15 "$LAP" 'ver' >/dev/null 2>&1 \
  || die "laptop unreachable"
ssh -o BatchMode=yes "$LAP" 'if not exist C:\Coins\TRI-LAPTOP\node\state exit 1' \
  || die "destination dir missing"

# ---- 1. STOP the GUI task (welded build: node is IN-PROCESS; GUI answers RPC 19114) ----
# best-effort end + kill, then FAIL CLOSED unless BOTH process names are gone
# (ssh success distinguished from empty result: tasklist exits 0; grep runs locally)
ssh -o BatchMode=yes "$LAP" 'schtasks /end /tn tri7gui-laptop >nul 2>&1 & taskkill /f /im tri7_wallet_gui.exe >nul 2>&1 & exit /b 0' || true
LEFT=""; GOT=""
for i in $(seq 1 10); do
  RAW=$(ssh -o BatchMode=yes "$LAP" 'tasklist' 2>/dev/null) && GOT=1 || { sleep 2; continue; }
  LEFT=$(printf '%s' "$RAW" | tr -d '\r' | grep -iE 'tri7_wallet|follow_tip' || true)
  [ -z "$LEFT" ] && break
  sleep 2
done
[ -n "$GOT" ] || die "process probe never succeeded over ssh"
[ -z "$LEFT" ] || die "tri7_wallet_gui/follow_tip still running after stop: $LEFT"

# ---- 2. RENAME ASIDE only the chain-state files ----
# 2a. reject pre-existing backup targets (abort if the probe itself fails)
PRE_RAW=$(ssh -o BatchMode=yes "$LAP" "if exist $SD\\blk0001.dat.pre-seedswap (echo A1) & if exist $SD\\tipstate.json.pre-seedswap (echo A2) & if exist $SD\\index.pre-seedswap (echo A3) & if exist $SD\\gap_recovery.json.pre-seedswap (echo A4)" 2>/dev/null) \
  || die "backup-target probe failed over ssh"
PRE=$(printf '%s' "$PRE_RAW" | tr -d '\r\n ')
[ -z "$PRE" ] || die "backup target(s) already exist: $PRE — aborting before any rename"
# 2b. verify ALL FOUR sources exist BEFORE mutating anything (no partial rename)
SRC_OK_RAW=$(ssh -o BatchMode=yes "$LAP" "if exist $SD\\blk0001.dat (echo S1) & if exist $SD\\tipstate.json (echo S2) & if exist $SD\\index (echo S3) & if exist $SD\\gap_recovery.json (echo S4)" 2>/dev/null) \
  || die "source-existence probe failed over ssh"
SRC_OK=$(printf '%s' "$SRC_OK_RAW" | tr -d '\r\n ')
[ "$SRC_OK" = "S1S2S3S4" ] || die "expected source(s) missing before rename: [$SRC_OK]"
# 2c. each rename as its own checked command
for pair in "blk0001.dat blk0001.dat.pre-seedswap" "tipstate.json tipstate.json.pre-seedswap" "index index.pre-seedswap" "gap_recovery.json gap_recovery.json.pre-seedswap"; do
  set -- $pair
  ssh -o BatchMode=yes "$LAP" "ren $SD\\$1 $2" || die "rename failed: $1"
done
# 2d. REN-DONE only after every expected backup EXISTS and every source is GONE
CHK_RAW=$(ssh -o BatchMode=yes "$LAP" "if exist $SD\\blk0001.dat.pre-seedswap (echo B1) & if exist $SD\\tipstate.json.pre-seedswap (echo B2) & if exist $SD\\index.pre-seedswap (echo B3) & if exist $SD\\gap_recovery.json.pre-seedswap (echo B4) & if not exist $SD\\blk0001.dat (echo G1) & if not exist $SD\\tipstate.json (echo G2) & if not exist $SD\\index (echo G3) & if not exist $SD\\gap_recovery.json (echo G4)" 2>/dev/null) \
  || die "rename verification probe failed over ssh"
CHK=$(printf '%s' "$CHK_RAW" | tr -d '\r\n ')
[ "$CHK" = "B1B2B3B4G1G2G3G4" ] || die "rename verification incomplete: [$CHK]"
echo "REN-DONE (all four renamed aside and verified)"

# ---- 3. TRANSFER (separate scp calls; -r for index; ~15 min for 983 MB over tailscale) ----
scp -o BatchMode=yes "$SEED/blk0001.dat"   "$LAP:$DEST/blk0001.dat"   || die "scp blk0001.dat"
scp -o BatchMode=yes "$SEED/tipstate.json" "$LAP:$DEST/tipstate.json" || die "scp tipstate.json"
scp -o BatchMode=yes -r "$SEED/index"      "$LAP:$DEST/"              || die "scp index"

# ---- 4. READBACK HASH COMPARE (money-path state: byte identity, not size) ----
REMOTE=$(ssh -o BatchMode=yes "$LAP" 'powershell -NoProfile -Command "Get-FileHash -Algorithm SHA256 (\"C:\Coins\TRI-LAPTOP\node\state\blk0001.dat\",\"C:\Coins\TRI-LAPTOP\node\state\tipstate.json\",\"C:\Coins\TRI-LAPTOP\node\state\index\blockidx.bin\",\"C:\Coins\TRI-LAPTOP\node\state\index\txidx.bin\") | ForEach-Object { $_.Path + \" = \" + $_.Hash }"' 2>/dev/null); RC=$?
[ "$RC" -eq 0 ] || die "readback hash probe failed over ssh (rc=$RC)"
diff <(echo "$SRC_HASHES" | awk '{print tolower($1)}') \
     <(echo "$REMOTE" | tr -d '\r' | awk '/ = /{print tolower($NF)}') \
  || die "readback hash mismatch"

# ---- 5. RELAUNCH + bounded readiness poll (no fixed sleep) ----
ssh -o BatchMode=yes "$LAP" 'schtasks /run /tn tri7gui-laptop' || die "task start"
isnum(){ case "$1" in ''|*[!0-9]*) return 1;; *) return 0;; esac; }
LT=""; OK=0
for i in $(seq 1 40); do          # up to ~10 min: index load + wallet seed
  RAW=$(ssh -o BatchMode=yes "$LAP" "powershell -NoProfile -Command \"(Invoke-RestMethod -Uri http://127.0.0.1:19114 -Method Post -Body '{\\\"method\\\":\\\"getblockcount\\\"}' -ContentType 'application/json').result\"" 2>/dev/null); RC=$?
  if [ "$RC" -eq 0 ]; then
    LT=$(printf '%s' "$RAW" | tr -d '\r' | tail -1)
    if isnum "$LT" && [ "$LT" -gt 0 ]; then OK=1; break; fi
  fi
  sleep 15
done
[ "$OK" -eq 1 ] || die "laptop RPC not ready after 10 min (ssh rc=$RC, last='$LT')"
# second bounded observation: NON-REGRESSION check (tip non-decreasing — we
# claim liveness, not advancement; chain mints make > common but not required)
sleep 30
RAW2=$(ssh -o BatchMode=yes "$LAP" "powershell -NoProfile -Command \"(Invoke-RestMethod -Uri http://127.0.0.1:19114 -Method Post -Body '{\\\"method\\\":\\\"getblockcount\\\"}' -ContentType 'application/json').result\"" 2>/dev/null); RC=$?
[ "$RC" -eq 0 ] || die "second tip probe failed over ssh (rc=$RC)"
LT2=$(printf '%s' "$RAW2" | tr -d '\r' | tail -1)
isnum "$LT2" || die "second tip observation failed ('$LT2')"
[ "$LT2" -ge "$LT" ] || die "tip regressed between observations ($LT -> $LT2)"
ADV=stable; [ "$LT2" -gt "$LT" ] && ADV=advancing
LT=$LT2   # report the fresher tip
DT=$(curl -sS --max-time 6 -X POST http://127.0.0.1:19112 -d '{"method":"getblockcount"}' | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"])') \
  || die "DNS2 tip probe failed"
isnum "$DT" || die "DNS2 tip non-numeric ('$DT')"
[ "$DT" -gt 0 ] || die "DNS2 tip is 0 — witness node unhealthy, refusing to compare"
SH=$(( LT < DT ? LT : DT ))
echo "laptop=$LT ($ADV) dns2=$DT shared_height=$SH"
LH_RAW=$(ssh -o BatchMode=yes "$LAP" "powershell -NoProfile -Command \"(Invoke-RestMethod -Uri http://127.0.0.1:19114 -Method Post -Body '{\\\"method\\\":\\\"getblockhash\\\",\\\"params\\\":[ $SH ]}' -ContentType 'application/json').result\"" 2>/dev/null); RC=$?
[ "$RC" -eq 0 ] || die "laptop hash probe failed over ssh (rc=$RC)"
LH=$(printf '%s' "$LH_RAW" | tr -d '\r' | tail -1)
DH=$(curl -sS --max-time 6 -X POST http://127.0.0.1:19112 -d "{\"method\":\"getblockhash\",\"params\":[$SH]}" | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"])') \
  || die "DNS2 hash probe failed"
[ -n "$LH" ] && [ "$LH" = "$DH" ] && echo "PARITY OK at h$SH: $DH" \
  || die "PARITY FAIL laptop=$LH dns2=$DH"
echo "SWAP COMPLETE — laptop at tip $LT ($ADV; non-regression verified; parity at h$SH)"

FRONTMATTER (a separate mechanical step applied AFTER this grade lands):
codex_reviewed_at 2026-11-05 -> 2026-11-10 and codex_review_id -> the verdict
urn minted by the grade that approves this patch. No SKILL.md body change.
