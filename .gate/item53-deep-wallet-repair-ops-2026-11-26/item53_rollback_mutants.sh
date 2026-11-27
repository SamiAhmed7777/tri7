#!/usr/bin/env bash
# ev: do the rollback sandbox's checks catch a broken rollback? Each mutant
# must make exactly its expected checks fail.
set -u
GATE=$(dirname "$(readlink -f "$0")")
ORIG=${ITEM53_RB_UNDER_TEST:-$GATE/item53_rollback.sh}
H=$GATE/item53_rollback_sandbox.sh
M=$(mktemp -d /root/.hermes/cache/scratch/item53_rb_mut.XXXXXX)
bad=0
mutant() {  # mutant <name> <old=>new> <expected failing checks, '|' separated>
  local name=$1 repl=$2 expected=$3
  python3 - "$ORIG" "$M/$name.sh" "$repl" <<'PY'
import sys
src, dst, repl = sys.argv[1:4]
t = open(src).read(); old, new = repl.split("=>", 1)
assert t.count(old) == 1, "the line to mutate is not there exactly once"
open(dst, "w").write(t.replace(old, new))
PY
  [ $? = 0 ] || { echo "MUTANT $name: could not be built"; bad=1; return; }
  ITEM53_RB_UNDER_TEST="$M/$name.sh" bash "$H" > "$M/$name.out" 2>&1
  local rc=$?
  local got; got=$(grep '^FAIL' "$M/$name.out" | sed 's/^FAIL  //' | sed 's/ (got [0-9]*)$//' | paste -sd'|')
  if [ "$rc" = 1 ] && [ "$got" = "$expected" ]; then echo "KILLED   $name — failing checks: $(echo "$got" | sed 's/|/; /g')"
  else echo "SURVIVED $name (harness exit $rc; failing: $got)"; bad=1; fi
}
echo "# ev: mutants of item53_rollback.sh against the sandbox (sha256 of the original: $(sha256sum "$ORIG" | cut -c1-64)) — $(date '+%Y-%m-%dT%H:%M:%S %Z')"
mutant no_final_hash_check \
  '|| rollback_failed "the restored index does not hash to the backup"=>|| true' \
  'D exit 2|D says which check failed|D nothing was started after the failure'
mutant stale_wal_left_in_place \
  'rm -f "$LIVE_DB-wal" "$LIVE_DB-shm"
=> 
' \
  'A no restore file or stale wal left'
mutant daemon_stopped_first \
  'say "stopping $DAEMON"
"$SC" stop "$DAEMON" || abort_before_swap "could not stop the daemon"
wait_inactive "$DAEMON" 120 || abort_before_swap "the daemon was still active after 120 s"
=> 
' \
  'A order: timer stop, daemon stop, daemon start, timer start, catch-up'
mutant prepared_copy_not_hashed \
  'refuse "the prepared copy'"'"'s sha256 is not the backup'"'"'s"=>true' \
  'H exit 1|H nothing was stopped or started'
mutant check_mode_swaps \
  'if [ "${1:-}" = "--check" ]; then=>if false; then' \
  'I exit 0'
mutant check_arg_typo_leaves_copy \
  '[ "$#" = 0 ] || { rm -f "$restore"; refuse=>[ "$#" = 0 ] || { refuse' \
  'J no restore file left'
mutant chown_failure_ignored \
  'chown "$chown_to" "$LIVE_DB" || swap_fix_failed=>chown "$chown_to" "$LIVE_DB" || true #' \
  'K exit 2|K nothing was started after the failure|K says which step failed|K prints the hand-fix'
mutant chmod_failure_ignored \
  'chmod "$chmod_to" "$LIVE_DB" || swap_fix_failed=>chmod "$chmod_to" "$LIVE_DB" || true #' \
  'L exit 2|L nothing was started after the failure|L says which step failed|L prints the hand-fix'
mutant live_path_guard_removed \
  'if [ "${ITEM53_SANDBOX:-}" = 1 ] && [ -n "${ITEM53_TEST_FAULT:-}" ] && [ "$LIVE_DB" = "$DEFAULT_LIVE_DB" ]; then=>if false; then' \
  'M names the guard and the live path|M refused before the lock was taken'
rm -rf "$M"
# the un-mutated script against the same sandbox, for the record
ITEM53_RB_UNDER_TEST="$ORIG" bash "$H" | grep -E '^(# [0-9]+ checks|# EXIT|FAIL)'
echo "# EXIT-STATUS: $bad"
exit $bad
