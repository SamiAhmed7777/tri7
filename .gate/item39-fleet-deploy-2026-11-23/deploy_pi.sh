#!/usr/bin/env bash
# ITEM 39 fleet deploy — Pi leg. Executed ON pi (ssh hetzner) via `ssh hetzner 'bash -s -- <args>'`.
# argv: $1 = FULL commit hash to build (pinned + verified post-checkout), $2 = sha256 of the
#       currently-RUNNING binary (rollback target, full), $3 = min tip height floor
# Exit codes: 0 = deployed + healthy; 1 = clean rollback (binary AND repo state restored,
#             digest- and HEAD-verified); 2 = FATAL rollback/restoration failed;
#             3 = aborted BEFORE the live binary path, daemon, or repository content was
#             mutated (the only possible lasting effects of a 3 are an empty backup
#             directory and/or a digest-verified backup copy of the still-running bytes).
# Phase discipline: the ERR->rollback trap is armed ONLY after a digest-verified backup of
# the running bytes exists and ORIG_HEAD/ORIG_BRANCH are captured; before that, every
# failure path is explicitly guarded and exits 3 — rollback can never run with unset or
# unverified restoration inputs.
# Operator invocation (from DNS2):
#   ssh hetzner 'bash -s -- 574723943b7f39ef3276e9a57beaf4acb5d99258 \
#     f027bd5dd59d4608493d404f83f6613cefaa50098cf9dc5f2fd1b81b528f23db 2217775' \
#     < deploy_pi.sh
set -Eeuo pipefail

COMMIT="${1:?usage: bash -s -- <full-commit-hash> <running-binary-sha256-full> <min-tip-height>}"
RUN_SHA="${2:?running binary sha256 (full)}"
MIN_TIP="${3:?min tip height}"
UNIT="${ITEM39_UNIT:-tri7d}"
REPO="${ITEM39_REPO:-/root/tri7}"
BIN="$REPO/target/release/follow_tip"
BACKUP="${ITEM39_BACKUP:-/root/backups/follow_tip.f027bd5d.bak-item39}"
RPC="${ITEM39_RPC:-http://127.0.0.1:19112}"
CONF="${ITEM39_CONF:-/etc/tri7/triangles.conf}"

# ---------- PHASE 0: read-only pre-flight. No ERR trap; failures exit 3, nothing mutated. ----------
for c in sha256sum systemctl python3 git cargo install tail mkdir cut rm timeout; do
  command -v "$c" >/dev/null 2>&1 || { echo "FATAL: missing command $c"; exit 3; }
done
[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || { echo "ABORT: commit must be a full 40-hex hash, got: $COMMIT"; exit 3; }
[[ "$RUN_SHA" =~ ^[0-9a-f]{64}$ ]] || { echo "ABORT: running-binary sha not a full 64-hex digest: $RUN_SHA"; exit 3; }
[[ "$MIN_TIP" =~ ^[0-9]+$ ]] || { echo "ABORT: min-tip not numeric: $MIN_TIP"; exit 3; }

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

systemctl is-active --quiet "$UNIT" || { echo "ABORT: $UNIT not active before deploy"; exit 3; }
# Prove the RUNNING INODE's bytes are the ones we are replacing (ExecStart path == running bytes)
MAINPID="$(systemctl show "$UNIT" -p MainPID --value)"
[[ "$MAINPID" =~ ^[0-9]+$ ]] && [ "$MAINPID" != "0" ] || { echo "ABORT: no MainPID for $UNIT"; exit 3; }
CUR_SHA="$(sha256sum "$BIN" 2>/dev/null | cut -d' ' -f1)" || true
[ -n "$CUR_SHA" ] || { echo "ABORT: cannot hash live binary path $BIN"; exit 3; }
[ "$CUR_SHA" = "$RUN_SHA" ] || { echo "ABORT: live binary digest $CUR_SHA != expected $RUN_SHA"; exit 3; }
INODE_SHA="$(sha256sum "/proc/$MAINPID/exe" 2>/dev/null | cut -d' ' -f1)" || true
[ -n "$INODE_SHA" ] || { echo "ABORT: cannot hash running inode /proc/$MAINPID/exe"; exit 3; }
[ "$INODE_SHA" = "$RUN_SHA" ] || { echo "ABORT: running inode sha ${INODE_SHA:0:8} != expected $RUN_SHA — ExecStart path is not the running bytes"; exit 3; }

# Repo pre-flight: capture the CURRENT ref state BEFORE any mutation (all read-only).
# A dirty working tree would make "built from the pinned commit" unverifiable, and submodules
# are outside this script's restore guarantees — both abort here, nothing touched.
ORIG_HEAD="$(git -C "$REPO" rev-parse HEAD 2>/dev/null)" || { echo "ABORT: $REPO is not a usable git repository"; exit 3; }
[ -n "$ORIG_HEAD" ] || { echo "ABORT: rev-parse HEAD empty"; exit 3; }
ORIG_BRANCH="$(git -C "$REPO" symbolic-ref -q --short HEAD 2>/dev/null || true)"
[ -z "$(git -C "$REPO" status --porcelain 2>/dev/null)" ] || { echo "ABORT: repo working tree not clean — refusing to deploy over operator changes"; exit 3; }
[ ! -f "$REPO/.gitmodules" ] || { echo "ABORT: repo uses submodules — outside this script's guarantees"; exit 3; }
echo "pre-state: unit active (pid $MAINPID), running+path binary sha ${RUN_SHA:0:8}, repo at ${ORIG_HEAD:0:8}${ORIG_BRANCH:+ (branch $ORIG_BRANCH)}, tree clean"

# ---------- restore helpers (used only after Phase 1 begins) ----------
restore_repo() {
  if [ -n "${ORIG_BRANCH:-}" ]; then
    git -C "$REPO" checkout --quiet "$ORIG_BRANCH"
  else
    git -C "$REPO" checkout --quiet --detach "$ORIG_HEAD"
  fi
}

# Proven restoration for PRE-RESTART failure paths: any failed restoration is FATAL exit 2.
restore_pre_restart_or_fatal() {
  install -m 0755 "$BACKUP" "$BIN" || { echo "FATAL: RESTORATION FAILED (binary path install)"; exit 2; }
  [ "$(sha256sum "$BIN" | cut -d' ' -f1)" = "$RUN_SHA" ] || { echo "FATAL: RESTORATION FAILED (digest mismatch after restore)"; exit 2; }
  restore_repo || { echo "FATAL: RESTORATION FAILED (repo HEAD restore)"; exit 2; }
  [ "$(git -C "$REPO" rev-parse HEAD)" = "$ORIG_HEAD" ] || { echo "FATAL: RESTORATION FAILED (repo HEAD verify)"; exit 2; }
  echo "restored: binary path digest-proven == ${RUN_SHA:0:8}, repo back at ${ORIG_HEAD:0:8}"
}

rollback() {
  trap - ERR
  echo "ROLLBACK: restoring $BACKUP -> $BIN and repo HEAD"
  if ! install -m 0755 "$BACKUP" "$BIN"; then echo "FATAL: ROLLBACK FAILED (install)"; exit 2; fi
  local got; got="$(sha256sum "$BIN" | cut -d' ' -f1)"
  if [ "$got" != "$(sha256sum "$BACKUP" | cut -d' ' -f1)" ]; then
    echo "FATAL: ROLLBACK FAILED (digest mismatch after restore)"; exit 2
  fi
  if ! restore_repo; then echo "FATAL: ROLLBACK FAILED (repo HEAD restore)"; exit 2; fi
  [ "$(git -C "$REPO" rev-parse HEAD)" = "$ORIG_HEAD" ] || { echo "FATAL: ROLLBACK FAILED (repo HEAD verify)"; exit 2; }
  systemctl restart "$UNIT" || { echo "FATAL: ROLLBACK FAILED (restart)"; exit 2; }
  if ! systemctl is-active --quiet "$UNIT"; then echo "FATAL: ROLLBACK FAILED (unit inactive after restart)"; exit 2; fi
  # prove the RESTORED bytes are what is now RUNNING (not just the pathname)
  local MPID RIN; MPID="$(systemctl show "$UNIT" -p MainPID --value)"
  [[ "$MPID" =~ ^[0-9]+$ ]] && [ "$MPID" != "0" ] || { echo "FATAL: ROLLBACK FAILED (no MainPID after restart)"; exit 2; }
  RIN="$(sha256sum "/proc/$MPID/exe" 2>/dev/null | cut -d' ' -f1)"
  [ -n "$RIN" ] || { echo "FATAL: ROLLBACK FAILED (cannot hash running inode /proc/$MPID/exe)"; exit 2; }
  [ "$RIN" = "$RUN_SHA" ] || { echo "FATAL: ROLLBACK FAILED (running inode sha ${RIN:0:8} != restored ${RUN_SHA:0:8})"; exit 2; }
  echo "ROLLBACK: binary + repo state restored, unit active, path digest AND running inode proven == ${RUN_SHA:0:8}. Exit 1."
  exit 1
}

# ---------- PHASE 1: mutation begins. Backup dir, then verified backup, THEN arm the trap. ----------
if ! mkdir -p "$(dirname "$BACKUP")" 2>/dev/null; then
  echo "ABORT: cannot create backup directory $(dirname "$BACKUP")"; exit 3
fi
if ! install -m 0755 "$BIN" "$BACKUP"; then
  rm -f "$BACKUP"; echo "ABORT: backup copy failed (removed; target untouched)"; exit 3
fi
[ "$(sha256sum "$BACKUP" | cut -d' ' -f1)" = "$RUN_SHA" ] || {
  rm -f "$BACKUP";   # remove the bad side copy: live binary/daemon/repo untouched
  echo "ABORT: backup digest mismatch (backup removed; target untouched)"; exit 3; }
echo "backup proven: $BACKUP == ${RUN_SHA:0:8}"

# NOW every restoration input exists and is verified — arm the trap.
trap 'echo "DEPLOY FAILED at line $LINENO"; rollback' ERR

# --- source: fetch + checkout the pinned graded commit; VERIFY HEAD equals it exactly.
#     These are PRE-RESTART phases: failures are explicitly guarded (NOT left to the ERR
#     trap) so the daemon is never restarted for them — restore + prove + exit 3. ---
if ! git -C "$REPO" fetch origin --quiet; then
  echo "FETCH FAILED — remote refs only; working tree untouched; proving restore and exiting 3 (no restart)"
  restore_pre_restart_or_fatal
  exit 3
fi
if ! git -C "$REPO" checkout --quiet --detach "$COMMIT"; then
  echo "CHECKOUT FAILED — restoring binary path + repo state (daemon untouched, no restart)"
  restore_pre_restart_or_fatal
  exit 3
fi
GOT_HEAD="$(git -C "$REPO" rev-parse HEAD)"
[ "$GOT_HEAD" = "$COMMIT" ] || {
  echo "ABORT: HEAD $GOT_HEAD != pinned commit $COMMIT";
  restore_pre_restart_or_fatal;
  exit 3; }
[ -z "$(git -C "$REPO" status --porcelain)" ] || {
  echo "ABORT: working tree not clean at pinned commit — build provenance unverifiable";
  restore_pre_restart_or_fatal;
  exit 3; }
echo "repo: detached at $(git -C "$REPO" rev-parse --short HEAD), verified == pinned $COMMIT, tree clean (tracked content == $COMMIT)"

# --- build (aarch64 on-device). Failure = exit 3 AFTER PROVEN restoration of binary path AND
#     repo state; the daemon keeps running the OLD inode — no restart happens on build failure. ---
if ! cargo build --release --bin follow_tip --manifest-path "$REPO/Cargo.toml" 2>&1 | tail -5; then
  echo "BUILD FAILED — restoring binary path + repo state (daemon untouched, still running old inode)"
  restore_pre_restart_or_fatal
  exit 3
fi
[ -x "$BIN" ] || {
  echo "BUILD FAILED — binary absent; restoring binary path + repo state";
  restore_pre_restart_or_fatal;
  exit 3; }
NEW_SHA="$(sha256sum "$BIN" | cut -d' ' -f1)"
[ -n "$NEW_SHA" ] && [ "$NEW_SHA" != "$RUN_SHA" ] || {
  echo "ABORT: rebuilt binary digest unchanged/absent ($NEW_SHA)";
  restore_pre_restart_or_fatal;
  exit 3; }
echo "built: new aarch64 binary sha ${NEW_SHA:0:8} (x86_64 fleet sha differs by design — arch)"

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
  # prove the RUNNING INODE is the newly built binary (not just the path)
  MAINPID2="$(systemctl show "$UNIT" -p MainPID --value)"
  RUN2="$(sha256sum "/proc/$MAINPID2/exe" 2>/dev/null | cut -d' ' -f1)" || RUN2=""
  if [ "$RUN2" != "$NEW_SHA" ]; then
    echo "running inode sha ${RUN2:0:8} != built ${NEW_SHA:0:8}"
    rollback
  fi
  echo "DEPLOYED: new sha ${NEW_SHA:0:8}, running inode == built binary, repo pinned at $COMMIT, unit restarted, RPC healthy ($HEALTH)"; exit 0;; *) rollback;; esac
