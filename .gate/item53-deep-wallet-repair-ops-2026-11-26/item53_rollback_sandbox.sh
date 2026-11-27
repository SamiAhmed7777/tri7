#!/usr/bin/env bash
# ev: sandbox test of item53_rollback.sh. Nothing live is touched: every path
# and every command the script uses is pointed at this scratch directory.
# The one exception is scenario M, which exercises the script's live-path guard
# with a backup whose pinned sha256 is wrong, so that the run is refused twice
# over before anything is touched (see M below).
# Prints one PASS/FAIL line per check and exits 1 if any check failed.
#
#   bash item53_rollback_sandbox.sh                     tests the script beside it
#   ITEM53_RB_UNDER_TEST=<path> bash item53_rollback_sandbox.sh   tests another copy
set -u
SBX=$(mktemp -d /root/.hermes/cache/scratch/item53_rb_sandbox.XXXXXX)
RB=${ITEM53_RB_UNDER_TEST:-$(dirname "$(readlink -f "$0")")/item53_rollback.sh}
mkdir -p "$SBX/bin"
fails=0; checks=0
chk() {  # chk <ok:0/1> <what>
  checks=$((checks + 1))
  if [ "$1" = 0 ]; then echo "PASS  $2"; else echo "FAIL  $2"; fails=$((fails + 1)); fi
}
export SBX

# a fake systemctl: records every call, and fails where a fault file says so
cat > "$SBX/bin/systemctl" <<'EOF'
#!/bin/bash
echo "$*" >> "${SBX_LOG:-$SBX/systemctl.log}"
case "$1" in
  is-active)
    if [ -f "$SBX/catchup_never_stops" ] || { [ -f "$SBX/catchup_running" ] && [ ! -f "$SBX/catchup_done" ]; }; then
      n=$(cat "$SBX/polls" 2>/dev/null || echo 0); n=$((n + 1)); echo "$n" > "$SBX/polls"
      if [ -n "${SBX_CATCHUP_POLLS:-}" ] && [ "$n" -ge "$SBX_CATCHUP_POLLS" ]; then touch "$SBX/catchup_done"; exit 3; fi
      exit 0
    fi
    exit 3 ;;
  start)
    case "$2" in
      tri7d.service) [ -f "$SBX/fault_daemon_start" ] && exit 1 || exit 0 ;;
      tri7-addrindex.service) [ -f "$SBX/fault_catchup_start" ] && exit 1 || exit 0 ;;
      *) exit 0 ;;
    esac ;;
  show) if [ -f "$SBX/fault_catchup_result" ]; then echo failed; else echo success; fi ;;
  *) exit 0 ;;
esac
EOF
# a fake rpc probe: prints a block count unless a fault file says otherwise
cat > "$SBX/bin/rpc_probe" <<'EOF'
#!/bin/bash
[ -f "$SBX/fault_rpc" ] && exit 1
echo 2220400
EOF
chmod +x "$SBX/bin/"*

# two small databases: the pristine backup and a different "applied" one
python3 - "$SBX" <<'EOF'
import sqlite3, sys
s = sys.argv[1]
import os
os.makedirs(s + "/backup")
for name, val in (("backup/addressindex.db", "pre-repair"), ("applied.db", "applied")):
    c = sqlite3.connect(s + "/" + name)
    c.execute("create table out (txid text, n integer, spent_height integer)")
    c.executemany("insert into out values ('tx" + val + "', ?, ?)", [(i, None) for i in range(5)])
    c.commit(); c.close()
EOF
BACKUP_SHA=$(sha256sum "$SBX/backup/addressindex.db" | awk '{print $1}')
APPLIED_SHA=$(sha256sum "$SBX/applied.db" | awk '{print $1}')
sha_of() { sha256sum "$1" 2>/dev/null | awk '{print $1}'; }

# prep <name>: a fresh live index in the "applied" state, no faults
prep() {
  D="$SBX/$1"
  mkdir -p "$D/live"
  rm -f "$D/systemctl.log" "$D/polls" "$SBX/systemctl.log" "$SBX/polls" "$SBX"/catchup_* "$SBX"/fault_* "$D"/live/*
  cp "$SBX/applied.db" "$D/live/addressindex.db"
  printf 'wal' > "$D/live/addressindex.db-wal"
  EXTRA_ENV=()
}
# go: run the rollback against the scenario's paths (EXTRA_ENV set by the caller)
go() {
  ( cd "$SBX" && env ITEM53_BACKUP_DIR="$SBX/backup" ITEM53_BACKUP_SHA256="$BACKUP_SHA" \
      ITEM53_LIVE_DB="$D/live/addressindex.db" \
      ITEM53_SYSTEMCTL="$SBX/bin/systemctl" ITEM53_RPC_PROBE="$SBX/bin/rpc_probe" \
      ITEM53_LOCK="$D/lock" SBX_LOG="$D/systemctl.log" "${EXTRA_ENV[@]}" bash "$RB" ${ITEM53_RB_ARGS:-} > "$D/out.txt" 2>&1 )
  RC=$?
  LIVE_SHA=$(sha_of "$D/live/addressindex.db")
}
stops_starts() { grep -E '^(stop|start) ' "$D/systemctl.log" 2>/dev/null | tr '\n' ';'; }

echo "# ev: sandbox test of item53_rollback.sh — $(date '+%Y-%m-%dT%H:%M:%S %Z')"
echo "# sandbox: a scratch directory; fake systemctl and rpc probe; two small databases"
echo "# script under test: sha256 $(sha_of "$RB")"

# A: the normal path, with a catch-up that is still running for two polls
prep A; touch "$SBX/catchup_running"; EXTRA_ENV=(SBX_CATCHUP_POLLS=2); go
chk $([ "$RC" = 0 ] && echo 0 || echo 1) "A exit 0 (got $RC)"
chk $([ "$LIVE_SHA" = "$BACKUP_SHA" ] && echo 0 || echo 1) "A live index == the backup"
chk $([ "$(sha_of "$D/live/addressindex.db.item53-applied")" = "$APPLIED_SHA" ] && echo 0 || echo 1) "A the applied state is kept beside it"
chk $([ ! -e "$D/live/addressindex.db.restore" ] && [ ! -e "$D/live/addressindex.db-wal" ] && echo 0 || echo 1) "A no restore file or stale wal left"
chk $([ "$(grep -c 'is-active --quiet tri7-addrindex.service' "$D/systemctl.log")" -ge 2 ] && echo 0 || echo 1) "A waited for the running catch-up to end"
chk $([ "$(stops_starts)" = "stop tri7-addrindex.timer;stop tri7d.service;start tri7d.service;start tri7-addrindex.timer;start tri7-addrindex.service;" ] && echo 0 || echo 1) "A order: timer stop, daemon stop, daemon start, timer start, catch-up"
grep -q 'ROLLBACK DONE AND PROVEN' "$D/out.txt"; chk $? "A prints the proof line"

# B: the backup is not the pinned one -> refused, nothing touched
prep B; EXTRA_ENV=(ITEM53_BACKUP_SHA256=0000000000000000000000000000000000000000000000000000000000000000); go
chk $([ "$RC" = 1 ] && echo 0 || echo 1) "B exit 1 (got $RC)"
chk $([ "$LIVE_SHA" = "$APPLIED_SHA" ] && echo 0 || echo 1) "B live index untouched"
chk $([ ! -e "$D/systemctl.log" ] && echo 0 || echo 1) "B systemctl was never called"
chk $([ ! -e "$D/live/addressindex.db.restore" ] && echo 0 || echo 1) "B no restore file left"

# C: something is in the way of the prepared file -> refused, nothing touched
prep C; mkdir -p "$D/live/addressindex.db.restore"; go
chk $([ "$RC" = 1 ] && echo 0 || echo 1) "C exit 1 (got $RC)"
chk $([ "$LIVE_SHA" = "$APPLIED_SHA" ] && [ -d "$D/live/addressindex.db.restore" ] && echo 0 || echo 1) "C live index untouched, the thing in the way not removed"

# D: the swapped-in file does not hash right -> exit 2, nothing started
prep D; EXTRA_ENV=(ITEM53_SANDBOX=1 ITEM53_TEST_FAULT=post_swap_sha); go
chk $([ "$RC" = 2 ] && echo 0 || echo 1) "D exit 2 (got $RC)"
grep -q 'ROLLBACK FAILED: the restored index does not hash' "$D/out.txt"; chk $? "D says which check failed"
chk $([ "$(grep -c '^start ' "$D/systemctl.log")" = 0 ] && echo 0 || echo 1) "D nothing was started after the failure"

# E: the daemon does not come back -> exit 2 (after a good restore)
prep E; touch "$SBX/fault_daemon_start"; go
chk $([ "$RC" = 2 ] && echo 0 || echo 1) "E exit 2 (got $RC)"
chk $([ "$LIVE_SHA" = "$BACKUP_SHA" ] && echo 0 || echo 1) "E the index was restored before that"
grep -q 'ROLLBACK FAILED: the daemon did not start' "$D/out.txt"; chk $? "E names the daemon"

# F: a catch-up that never ends -> exit 2, the daemon is NOT stopped, nothing swapped
prep F; EXTRA_ENV=(ITEM53_CATCHUP_WAIT=2); touch "$SBX/catchup_never_stops"; go
chk $([ "$RC" = 2 ] && echo 0 || echo 1) "F exit 2 (got $RC)"
chk $([ "$(grep -c 'stop tri7d.service' "$D/systemctl.log")" = 0 ] && echo 0 || echo 1) "F the daemon was not stopped"
chk $([ "$LIVE_SHA" = "$APPLIED_SHA" ] && [ ! -e "$D/live/addressindex.db.restore" ] && echo 0 || echo 1) "F live index untouched, prepared file removed"
grep -q 'ROLLBACK FAILED: the catch-up service was still active' "$D/out.txt"; chk $? "F names the catch-up"

# G: the daemon starts but its RPC never answers -> exit 2
prep G; EXTRA_ENV=(ITEM53_RPC_WAIT=2); touch "$SBX/fault_rpc"; go
chk $([ "$RC" = 2 ] && echo 0 || echo 1) "G exit 2 (got $RC)"
grep -q "ROLLBACK FAILED: the daemon's RPC did not answer" "$D/out.txt"; chk $? "G names the RPC wait"

# H: the prepared copy does not hash right -> refused, nothing touched
prep H; EXTRA_ENV=(ITEM53_SANDBOX=1 ITEM53_TEST_FAULT=copy_corrupt); go
chk $([ "$RC" = 1 ] && echo 0 || echo 1) "H exit 1 (got $RC)"
chk $([ "$LIVE_SHA" = "$APPLIED_SHA" ] && echo 0 || echo 1) "H live index untouched"
chk $([ "$(grep -cE '^(stop|start) ' "$D/systemctl.log")" = 0 ] && echo 0 || echo 1) "H nothing was stopped or started"
chk $([ ! -e "$D/live/addressindex.db.restore" ] && echo 0 || echo 1) "H no restore file left"

# I: --check mode on a healthy setup: exit 0, nothing stopped, nothing swapped
prep I; EXTRA_ENV=(); ITEM53_RB_ARGS=--check go; ITEM53_RB_ARGS=
chk $([ "$RC" = 0 ] && echo 0 || echo 1) "I exit 0 (got $RC)"
chk $([ "$LIVE_SHA" = "$APPLIED_SHA" ] && echo 0 || echo 1) "I live index untouched"
chk $([ ! -e "$D/systemctl.log" ] || [ "$(grep -cE '^(stop|start) ' "$D/systemctl.log")" = 0 ] && echo 0 || echo 1) "I nothing was stopped or started"
chk $([ ! -e "$D/live/addressindex.db.restore" ] && echo 0 || echo 1) "I no restore file left"

# J: a mistyped argument -> refused, nothing touched, no prepared copy left
prep J; EXTRA_ENV=(); ITEM53_RB_ARGS=--chek go; ITEM53_RB_ARGS=
chk $([ "$RC" = 1 ] && echo 0 || echo 1) "J exit 1 (got $RC)"
chk $([ "$LIVE_SHA" = "$APPLIED_SHA" ] && echo 0 || echo 1) "J live index untouched"
chk $([ ! -e "$D/systemctl.log" ] || [ "$(grep -cE '^(stop|start) ' "$D/systemctl.log")" = 0 ] && echo 0 || echo 1) "J nothing was stopped or started"
chk $([ ! -e "$D/live/addressindex.db.restore" ] && echo 0 || echo 1) "J no restore file left"

# K: the owner cannot be set after the swap -> exit 2, nothing started, the hand-fix printed
prep K; EXTRA_ENV=(ITEM53_SANDBOX=1 ITEM53_TEST_FAULT=post_swap_chown); go
chk $([ "$RC" = 2 ] && echo 0 || echo 1) "K exit 2 (got $RC)"
chk $([ "$LIVE_SHA" = "$BACKUP_SHA" ] && echo 0 || echo 1) "K the restored file is in place"
chk $([ "$(grep -c '^start ' "$D/systemctl.log")" = 0 ] && echo 0 || echo 1) "K nothing was started after the failure"
grep -q 'ROLLBACK FAILED: could not give it to' "$D/out.txt"; chk $? "K says which step failed"
grep -q 'Fix it by hand — chown ' "$D/out.txt"; chk $? "K prints the hand-fix"

# L: the mode cannot be set after the swap -> the same
prep L; EXTRA_ENV=(ITEM53_SANDBOX=1 ITEM53_TEST_FAULT=post_swap_chmod); go
chk $([ "$RC" = 2 ] && echo 0 || echo 1) "L exit 2 (got $RC)"
chk $([ "$LIVE_SHA" = "$BACKUP_SHA" ] && echo 0 || echo 1) "L the restored file is in place"
chk $([ "$(grep -c '^start ' "$D/systemctl.log")" = 0 ] && echo 0 || echo 1) "L nothing was started after the failure"
grep -q 'ROLLBACK FAILED: could not set mode ' "$D/out.txt"; chk $? "L says which step failed"
grep -q 'Fix it by hand — chmod ' "$D/out.txt"; chk $? "L prints the hand-fix"

# M: sandbox fault injection asked for on the LIVE index path -> refused by the
# guard, before the lock is taken. The default path is used on purpose, and the
# backup's pinned sha256 is wrong on purpose: if the guard ever failed to fire,
# the sha256 precondition would refuse the run next, before anything is touched.
prep M
( cd "$SBX" && env ITEM53_BACKUP_DIR="$SBX/backup" ITEM53_BACKUP_SHA256=0000000000000000000000000000000000000000000000000000000000000000 \
    ITEM53_SYSTEMCTL="$SBX/bin/systemctl" ITEM53_RPC_PROBE="$SBX/bin/rpc_probe" \
    ITEM53_LOCK="$D/lock" SBX_LOG="$D/systemctl.log" ITEM53_SANDBOX=1 ITEM53_TEST_FAULT=post_swap_sha \
    bash "$RB" > "$D/out.txt" 2>&1 )
RC=$?
chk $([ "$RC" = 1 ] && echo 0 || echo 1) "M exit 1 (got $RC)"
grep -q 'REFUSED: sandbox fault injection was asked for on the live index path (/var/lib/tri7/addressindex.db)' "$D/out.txt"; chk $? "M names the guard and the live path"
chk $([ ! -e "$D/lock" ] && echo 0 || echo 1) "M refused before the lock was taken"
chk $([ ! -e "$D/systemctl.log" ] && echo 0 || echo 1) "M systemctl was never called"

echo "# $checks checks, $fails failed"
echo "# the normal path's output (A):"
sed 's/^/#   /' "$SBX/A/out.txt"
echo "# a refused run's output (B):"
sed 's/^/#   /' "$SBX/B/out.txt"
echo "# a refused run's output (M):"
sed 's/^/#   /' "$SBX/M/out.txt"
echo "# a failed run's last lines (F):"
tail -3 "$SBX/F/out.txt" | sed 's/^/#   /'
echo "# a failed run's last lines (K):"
tail -4 "$SBX/K/out.txt" | sed 's/^/#   /'

rm -rf "$SBX"
echo "# EXIT-STATUS: $([ "$fails" = 0 ] && echo 0 || echo 1)"
exit $([ "$fails" = 0 ] && echo 0 || echo 1)
