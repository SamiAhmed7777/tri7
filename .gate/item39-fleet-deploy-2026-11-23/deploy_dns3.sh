#!/usr/bin/env bash
# ITEM 39 fleet deploy — DNS3 leg. Executed ON dns3 via `ssh dns3 'bash -s -- <args>'`.
# argv: $1 = staged binary path on this host, $2 = expected sha256 (full), $3 = min tip height floor
# Exit codes: 0 = deployed + healthy; 1 = clean rollback (digest-proven); 2 = FATAL
#             rollback/restoration failed; 3 = aborted BEFORE the live binary path or daemon
#             state was mutated (the only possible lasting effects of a 3 are an empty backup
#             directory and/or a digest-verified backup copy of the still-running bytes).
# Phase discipline: the ERR->rollback trap is armed ONLY after a digest-verified backup of
# the running bytes exists; before that, every failure path is explicitly guarded and exits 3
# — rollback can never run with an absent or unverified backup.
# Operator invocation (from DNS2):
#   scp /root/tri7/target/release/follow_tip dns3:/opt/tri7/bin/follow_tip.new-item39
#   ssh dns3 'bash -s -- /opt/tri7/bin/follow_tip.new-item39 \
#     87755353c853b96cfd01d5838d0c66f8f2cc5dc6993209b15287a86c9ea50c5e 2217775' \
#     < deploy_dns3.sh
set -Eeuo pipefail

STAGED="${1:?usage: bash -s -- <staged-binary> <expected-sha256-full> <min-tip-height>}"
WANT_SHA="${2:?expected sha256 (full)}"
MIN_TIP="${3:?min tip height}"
UNIT="${ITEM39_UNIT:-tri7d}"
BIN="${ITEM39_BIN:-/opt/tri7/bin/follow_tip}"
BACKUP="${ITEM39_BACKUP:-/opt/tri7/bin/follow_tip.bak-702b0517}"
RPC="${ITEM39_RPC:-http://127.0.0.1:19112}"
CONF="${ITEM39_CONF:-/etc/tri7/triangles.conf}"

# ---------- PHASE 0: read-only pre-flight. No ERR trap; failures exit 3, nothing mutated. ----------
for c in sha256sum systemctl python3 install cut rm timeout; do
  command -v "$c" >/dev/null 2>&1 || { echo "FATAL: missing command $c"; exit 3; }
done
[[ "$MIN_TIP" =~ ^[0-9]+$ ]] || { echo "ABORT: min-tip not numeric: $MIN_TIP"; exit 3; }
[[ "$WANT_SHA" =~ ^[0-9a-f]{64}$ ]] || { echo "ABORT: expected sha not a full 64-hex digest: $WANT_SHA"; exit 3; }

# RPC credentials must exist (read in-process, never printed)
python3 - "$CONF" <<'PYEOF' || { echo "ABORT: rpcuser/rpcpassword missing in $CONF"; exit 3; }
import sys
creds = {}
for line in open(sys.argv[1]):
    line = line.strip()
    if "=" in line and not line.startswith("#"):
        k, v = line.split("=", 1); creds[k.strip()] = v.strip()
sys.exit(0 if creds.get("rpcuser") and creds.get("rpcpassword") else 1)
PYEOF

[ -f "$STAGED" ] || { echo "ABORT: staged binary missing: $STAGED"; exit 3; }
GOT_SHA="$(sha256sum "$STAGED" 2>/dev/null | cut -d' ' -f1)" || true
[ -n "$GOT_SHA" ] || { echo "ABORT: cannot hash staged binary $STAGED"; exit 3; }
[ "$GOT_SHA" = "$WANT_SHA" ] || { echo "ABORT: staged digest $GOT_SHA != expected $WANT_SHA"; exit 3; }
[ -f "$BIN" ] || { echo "ABORT: live binary missing: $BIN"; exit 3; }
systemctl is-active --quiet "$UNIT" || { echo "ABORT: $UNIT not active before deploy"; exit 3; }
# Prove the RUNNING INODE's bytes are the ones we are replacing (ExecStart path == running bytes)
MAINPID="$(systemctl show "$UNIT" -p MainPID --value)"
[[ "$MAINPID" =~ ^[0-9]+$ ]] && [ "$MAINPID" != "0" ] || { echo "ABORT: no MainPID for $UNIT"; exit 3; }
OLD_SHA="$(sha256sum "$BIN" 2>/dev/null | cut -d' ' -f1)" || true
[ -n "$OLD_SHA" ] || { echo "ABORT: cannot hash live binary path $BIN"; exit 3; }
INODE_SHA="$(sha256sum "/proc/$MAINPID/exe" 2>/dev/null | cut -d' ' -f1)" || true
[ -n "$INODE_SHA" ] || { echo "ABORT: cannot hash running inode /proc/$MAINPID/exe"; exit 3; }
[ "$INODE_SHA" = "$OLD_SHA" ] || { echo "ABORT: running inode sha ${INODE_SHA:0:8} != path sha — ExecStart path is not the running bytes"; exit 3; }
echo "pre-state: unit active (pid $MAINPID), running+path binary sha ${OLD_SHA:0:8}, staged sha ${WANT_SHA:0:8}"

# ---------- restore helper (used only after Phase 1 begins) ----------
rollback() {
  trap - ERR
  echo "ROLLBACK: restoring $BACKUP -> $BIN"
  if ! install -m 0755 "$BACKUP" "$BIN"; then echo "FATAL: ROLLBACK FAILED (install)"; exit 2; fi
  local got; got="$(sha256sum "$BIN" | cut -d' ' -f1)"
  if [ "$got" != "$(sha256sum "$BACKUP" | cut -d' ' -f1)" ]; then
    echo "FATAL: ROLLBACK FAILED (digest mismatch after restore)"; exit 2
  fi
  systemctl restart "$UNIT" || { echo "FATAL: ROLLBACK FAILED (restart)"; exit 2; }
  if ! systemctl is-active --quiet "$UNIT"; then echo "FATAL: ROLLBACK FAILED (unit inactive after restart)"; exit 2; fi
  # prove the RESTORED bytes are what is now RUNNING (not just the pathname)
  local MPID RIN; MPID="$(systemctl show "$UNIT" -p MainPID --value)"
  [[ "$MPID" =~ ^[0-9]+$ ]] && [ "$MPID" != "0" ] || { echo "FATAL: ROLLBACK FAILED (no MainPID after restart)"; exit 2; }
  RIN="$(sha256sum "/proc/$MPID/exe" 2>/dev/null | cut -d' ' -f1)"
  [ -n "$RIN" ] || { echo "FATAL: ROLLBACK FAILED (cannot hash running inode /proc/$MPID/exe)"; exit 2; }
  [ "$RIN" = "$OLD_SHA" ] || { echo "FATAL: ROLLBACK FAILED (running inode sha ${RIN:0:8} != restored ${OLD_SHA:0:8})"; exit 2; }
  echo "ROLLBACK: restored backup, unit active, path digest AND running inode proven == ${OLD_SHA:0:8}. Exit 1."
  exit 1
}

# ---------- PHASE 1: mutation begins. Backup dir, then verified backup, THEN arm the trap. ----------
if ! mkdir -p "$(dirname "$BACKUP")" 2>/dev/null; then
  echo "ABORT: cannot create backup directory $(dirname "$BACKUP")"; exit 3
fi
if ! install -m 0755 "$BIN" "$BACKUP"; then
  rm -f "$BACKUP"; echo "ABORT: backup copy failed (removed; target untouched)"; exit 3
fi
[ "$(sha256sum "$BACKUP" | cut -d' ' -f1)" = "$OLD_SHA" ] || {
  rm -f "$BACKUP";   # remove the bad side copy: live binary/daemon untouched
  echo "ABORT: backup digest mismatch (backup removed; target untouched)"; exit 3; }
echo "backup proven: $BACKUP == ${OLD_SHA:0:8}"

# NOW the verified backup exists — arm the trap.
trap 'echo "DEPLOY FAILED at line $LINENO"; rollback' ERR

install -m 0755 "$STAGED" "$BIN"
NEW_GOT="$(sha256sum "$BIN" | cut -d' ' -f1)"
[ "$NEW_GOT" = "$WANT_SHA" ] || { echo "install digest mismatch: $NEW_GOT"; rollback; }

# --- restart + health poll: 120s monotonic poll budget inside a 130s HARD outer deadline
#     (coreutils timeout --kill-after SIGTERMs/SIGKILLs the poller at 130s regardless of
#     socket semantics); JSON-RPC errors rejected; hash validated ---
systemctl restart "$UNIT"
HEALTH="$(timeout --kill-after=10 130 python3 - "$RPC" "$CONF" "$MIN_TIP" <<'PYEOF'
import base64, json, sys, time, urllib.request
rpc, conf, min_tip = sys.argv[1], sys.argv[2], int(sys.argv[3])
creds = {}
for line in open(conf):
    line = line.strip()
    if "=" in line and not line.startswith("#"):
        k, v = line.split("=", 1); creds[k.strip()] = v.strip()
auth = base64.b64encode(f"{creds.get('rpcuser','')}:{creds.get('rpcpassword','')}".encode()).decode()

def call(method):
    body = json.dumps({"method": method, "params": []}).encode()
    req = urllib.request.Request(rpc, data=body, headers={
        "Content-Type": "application/json", "Authorization": "Basic " + auth})
    left = DEADLINE - time.monotonic()
    if left <= 0:
        raise TimeoutError("deadline")
    with urllib.request.urlopen(req, timeout=min(5.0, left)) as r:  # never above remaining budget
        out = json.loads(r.read())
    if out.get("error") is not None:
        raise ValueError(f"rpc-error: {out['error']}")
    return out.get("result")

DEADLINE = time.monotonic() + 120.0  # strict overall bound
last = "no-answer"
while time.monotonic() < DEADLINE:
    try:
        h = call("getblockcount")
        last = f"height={h}"
        if isinstance(h, int) and h >= min_tip:
            tip = call("getbestblockhash")
            if isinstance(tip, str) and len(tip) >= 32 and all(c in "0123456789abcdef" for c in tip):
                print(f"OK {last} tip={tip}")
                sys.exit(0)
            last = f"bad-tip={tip!r}"
    except Exception as e:
        last = f"err={e}"
    time.sleep(min(2.0, max(0.0, DEADLINE - time.monotonic())))  # capped by remaining budget
print(f"UNHEALTHY last={last}")
sys.exit(1)
PYEOF
)" || true
echo "health: $HEALTH"
case "$HEALTH" in OK\ *)
  # prove the RUNNING INODE is the newly installed binary (not just the path)
  MAINPID2="$(systemctl show "$UNIT" -p MainPID --value)"
  RUN2="$(sha256sum "/proc/$MAINPID2/exe" 2>/dev/null | cut -d' ' -f1)" || RUN2=""
  if [ "$RUN2" != "$NEW_GOT" ]; then
    echo "running inode sha ${RUN2:0:8} != installed ${NEW_GOT:0:8}"
    rollback
  fi
  echo "DEPLOYED: digest proven, running inode == installed binary, unit restarted, RPC healthy ($HEALTH)"; exit 0;; *) rollback;; esac
