#!/usr/bin/env bash
# Rollback of the item 53 deep wallet-row repair (DNS2 address index).
# Puts the pre-repair copy of addressindex.db back and PROVES it.
#
#   exit 0  restored and proven
#   exit 1  refused: a precondition failed. NOTHING was touched.
#   exit 2  ROLLBACK FAILED after the services were being stopped: read the
#           last lines. The timer and the daemon are then left as they are.
#
# The restored file is prepared and hash-verified BEFORE anything live is
# touched, so a failure before the swap changes nothing.
#
# usage: bash item53_rollback.sh            restore and prove
#        bash item53_rollback.sh --check    preconditions and the prepared copy only:
#                                          nothing is stopped, the prepared copy is removed
set -Eeuo pipefail

BACKUP_DIR=${ITEM53_BACKUP_DIR:-/root/tri7-lab/backups/item53-pre-dryrun-2026-11-26T0819}
BACKUP_DB="$BACKUP_DIR/addressindex.db"
# sha256 of the verified backup (ev_backup.txt)
BACKUP_SHA256=${ITEM53_BACKUP_SHA256:-8aa019a780aba9668978805a708a0174ae78185e201d8851b3e61663e8cb2015}
LIVE_DB=${ITEM53_LIVE_DB:-/var/lib/tri7/addressindex.db}
SC=${ITEM53_SYSTEMCTL:-systemctl}
RPC_PROBE=${ITEM53_RPC_PROBE:-$(dirname "$(readlink -f "$0")")/item53_rpc_probe.py}
LOCK=${ITEM53_LOCK:-/run/lock/item53-rollback.lock}
TIMER=tri7-addrindex.timer
CATCHUP=tri7-addrindex.service
DAEMON=tri7d.service
CATCHUP_WAIT=${ITEM53_CATCHUP_WAIT:-600}
RPC_WAIT=${ITEM53_RPC_WAIT:-120}

say() { printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S %Z')" "$*"; }
refuse() { say "REFUSED: $* — nothing was touched"; exit 1; }
rollback_failed() {
  trap - ERR
  say "ROLLBACK FAILED: $*"
  say "units: $(units_now); free space on the index's disk: $(free_mb) MiB"
  say "nothing further is done. Decide with the lines above before restarting anything."
  exit 2
}
sha() { sha256sum "$1" | awk '{print $1}'; }
free_mb() { df -Pk "$(dirname "$LIVE_DB")" | awk 'NR==2 {printf "%d", $4 / 1024}'; }
units_now() { printf 'timer %s, catch-up %s, daemon %s' "$("$SC" is-active "$TIMER" 2>/dev/null || true)" "$("$SC" is-active "$CATCHUP" 2>/dev/null || true)" "$("$SC" is-active "$DAEMON" 2>/dev/null || true)"; }
wait_inactive() {  # $1 = unit, $2 = seconds
  local i=0
  while "$SC" is-active --quiet "$1"; do
    i=$((i + 1))
    [ "$i" -le "$2" ] || return 1
    sleep 1
  done
  return 0
}

say "== rollback of the item 53 repair on DNS2"
exec 9>"$LOCK"
flock -n 9 || refuse "another rollback holds $LOCK"
for c in sha256sum cp mv rm ln stat df awk seq sleep flock readlink dirname "$SC"; do
  command -v "$c" >/dev/null 2>&1 || refuse "missing command: $c"
done
[ -f "$BACKUP_DB" ] || refuse "no backup at $BACKUP_DB"
got=$(sha "$BACKUP_DB")
[ "$got" = "$BACKUP_SHA256" ] || refuse "the backup's sha256 is not the pinned one"
[ -f "$LIVE_DB" ] || refuse "no live index at $LIVE_DB"
for u in "$TIMER" "$CATCHUP" "$DAEMON"; do
  "$SC" cat "$u" >/dev/null 2>&1 || refuse "no such unit: $u"
done
[ -x "$RPC_PROBE" ] || refuse "no rpc probe at $RPC_PROBE"
size=$(stat -c %s "$BACKUP_DB")
free_kb=$(df -Pk "$(dirname "$LIVE_DB")" | awk 'NR==2 {print $4}')
[ "$free_kb" -gt $((size / 1024 + 1048576)) ] || refuse "less than the backup's size plus 1 GiB free on the index's disk"
owner=$(stat -c '%U:%G' "$LIVE_DB")
mode=$(stat -c %a "$LIVE_DB")
say "preconditions ok: backup $size bytes, sha256 $got; $((free_kb / 1024)) MiB free"

# ---- prepare and verify the restored file; nothing live is touched yet
restore="$LIVE_DB.restore"
[ ! -e "$restore" ] || refuse "something is already in the way: $restore"
say "preparing the restored file next to the live one: $restore ($(free_mb) MiB free)"
if ! cp --reflink=auto "$BACKUP_DB" "$restore"; then
  rm -f "$restore"
  refuse "the copy failed ($(free_mb) MiB free)"
fi
sync
# sandbox harness only: both variables must be set; the live default path refuses
if [ "${ITEM53_SANDBOX:-}" = 1 ] && [ "${ITEM53_TEST_FAULT:-}" = copy_corrupt ]; then
  printf 'x' >> "$restore"
fi
got=$(sha "$restore")
if [ "$got" != "$BACKUP_SHA256" ]; then
  rm -f "$restore"
  refuse "the prepared copy's sha256 is not the backup's"
fi
say "prepared copy verified: sha256 $got ($(free_mb) MiB free)"
if [ "${1:-}" = "--check" ]; then
  rm -f "$restore"
  say "CHECK ONLY: nothing was stopped or swapped; the prepared copy is removed ($(free_mb) MiB free)"
  # the timer was left running, so a catch-up may be active here: that is the timer, not this check
  say "units as found now: $(units_now)"
  exit 0
fi
[ "$#" = 0 ] || { rm -f "$restore"; refuse "unknown argument '$1' (only --check exists)"; }

# ---- from here on a failure leaves the services stopped (exit 2)
abort_before_swap() {  # nothing live was swapped yet: drop the prepared copy, then stop
  rm -f "$restore" || true
  rollback_failed "$1"
}
say "stopping $TIMER (the catch-up is left to finish)"
"$SC" stop "$TIMER" || abort_before_swap "could not stop the timer"
wait_inactive "$CATCHUP" "$CATCHUP_WAIT" || abort_before_swap "the catch-up service was still active after ${CATCHUP_WAIT} s"
say "stopping $DAEMON"
"$SC" stop "$DAEMON" || abort_before_swap "could not stop the daemon"
wait_inactive "$DAEMON" 120 || abort_before_swap "the daemon was still active after 120 s"

say "swapping the file in ($(free_mb) MiB free)"
ln -f "$LIVE_DB" "$LIVE_DB.item53-applied" 2>/dev/null || true   # keeps the applied state for forensics; costs no space
rm -f "$LIVE_DB-wal" "$LIVE_DB-shm"
mv -f "$restore" "$LIVE_DB" || rollback_failed "could not move the restored file into place. The live index is UNCHANGED: starting $DAEMON and $TIMER again gives the service as it was"
chown "$owner" "$LIVE_DB" || true
chmod "$mode" "$LIVE_DB" || true
# sandbox harness only: both variables must be set; the live default path refuses
if [ "${ITEM53_SANDBOX:-}" = 1 ] && [ "${ITEM53_TEST_FAULT:-}" = post_swap_sha ]; then
  printf 'x' >> "$LIVE_DB"
fi
got=$(sha "$LIVE_DB")
[ "$got" = "$BACKUP_SHA256" ] || rollback_failed "the restored index does not hash to the backup"
say "restored index in place: sha256 $got, $owner $mode ($(free_mb) MiB free)"

say "starting $DAEMON"
"$SC" start "$DAEMON" || rollback_failed "the daemon did not start (start returned an error)"
count=""
for _ in $(seq 1 "$RPC_WAIT"); do
  count=$("$RPC_PROBE" 2>/dev/null || true)
  [ -n "$count" ] && break
  sleep 1
done
[ -n "$count" ] || rollback_failed "the daemon's RPC did not answer within ${RPC_WAIT} s"
say "daemon answers: block count $count"
say "starting $TIMER and running one catch-up"
"$SC" start "$TIMER" || rollback_failed "the timer did not start"
if ! "$SC" start "$CATCHUP"; then
  rollback_failed "the catch-up returned an error (see journalctl -u $CATCHUP)"
fi
result=$("$SC" show "$CATCHUP" -p Result --value)
[ "$result" = success ] || rollback_failed "the catch-up's Result is '$result'"
say "catch-up: Result=success"
say "ROLLBACK DONE AND PROVEN: index sha256 == the backup's, daemon answering, catch-up success"
exit 0
