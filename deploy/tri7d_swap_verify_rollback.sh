#!/usr/bin/env bash
#
# tri7d_swap_verify_rollback.sh — put a new tri7d binary in place, restart the
# unit, run an acceptance check, and put the previous binary back when the
# check does not pass. Run as root, ON the node.
#
#   tri7d_swap_verify_rollback.sh --bin <installed binary> --new <new binary> \
#        --check <acceptance script> [--unit tri7d] [--prev <saved previous binary>] \
#        [--wait-secs 900] [--rehearse-rollback]
#
#   --bin    the path the unit's ExecStart runs
#   --new    the binary to deploy. Same path as --bin means a build already
#            wrote the new binary in place; nothing is swapped then
#   --check  script run as `<check> <restart epoch>`, as root, on this host:
#              exit 0 = accept · exit 3 = still pending (polled every 10 s
#              until --wait-secs) · any other exit = reject
#   --prev   the previous binary, saved before an in-place build. Without it
#            the running binary is copied to <bin>.prev-item first
#   --rehearse-rollback   the real swap and restart, then a forced rejection,
#            to exercise the rollback path. Exit 0 only if the rollback put
#            the previous binary back in service
#
# Exit: 0 accepted (rehearsal: rollback restored) · 1 rejected and rolled
#       back (including a restart that failed after the swap — the failure is
#       caught, not left to `set -e`) · 2 usage or precondition failed
#       (nothing was changed except a saved <bin>.prev-item copy).
# The last stdout line is one JSON object with every observed fact.
# After an accepted deploy the previous binary stays at the --prev path.
# To roll back later:  mv -f <prev> <bin> && systemctl restart <unit>
#
# Rollback is not available when the reject arrives after the previous binary
# was replaced in place (--new == --bin) and no --prev was saved: the script
# refuses that combination before touching anything.
set -euo pipefail

BIN='' NEW='' CHECK='' PREV='' UNIT=tri7d WAIT=900 REHEARSE=0 SETTLE=45
need() { [[ $# -ge 2 ]] || { echo "$1 needs a value" >&2; exit 2; }; }
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin)    need "$@"; BIN=$2; shift 2 ;;
    --new)    need "$@"; NEW=$2; shift 2 ;;
    --check)  need "$@"; CHECK=$2; shift 2 ;;
    --prev)   need "$@"; PREV=$2; shift 2 ;;
    --unit)   need "$@"; UNIT=$2; shift 2 ;;
    --wait-secs) need "$@"; WAIT=$2; shift 2 ;;
    --rehearse-rollback) REHEARSE=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$BIN" && -n "$NEW" && -n "$CHECK" ]] || { sed -n '4,27p' "$0" >&2; exit 2; }
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 2; }

say() { printf '%s\n' "$*" >&2; }
sha() { sha256sum -- "$1" | cut -c1-64; }
emit() { # emit key=value ... → one JSON object (all values strings) on stdout
  local json='{' n=0 kv k v
  for kv in "$@"; do
    k=${kv%%=*}; v=${kv#*=}
    v=${v//\\/\\\\}; v=${v//\"/\\\"}; v=${v//$'\n'/ }
    (( n++ == 0 )) || json+=','
    json+="\"$k\":\"$v\""
  done
  printf '%s}\n' "$json"
}
running_sha() { # sha256 of the binary the unit's main process was started from
  local pid; pid=$(systemctl show -p MainPID --value "$UNIT")
  if [[ "$pid" =~ ^[1-9][0-9]*$ && -e "/proc/$pid/exe" ]]; then sha "/proc/$pid/exe"; else echo none; fi
}

# ---- preconditions: read-only, except one file save -----------------------------
# The only file touched before the swap is <bin>.prev-item (a copy of the
# running binary, made when --prev was not given) — and it happens BEFORE
# anything is stopped or renamed, so every refusal below leaves the host as it
# was except for that one saved copy.
[[ -x "$BIN" ]]   || { say "not an executable file: $BIN"; exit 2; }
[[ -x "$NEW" ]]   || { say "not an executable file: $NEW"; exit 2; }
[[ -x "$CHECK" ]] || { say "not an executable file: $CHECK"; exit 2; }
[[ "$WAIT" =~ ^[1-9][0-9]*$ ]] || { say "--wait-secs must be a positive integer"; exit 2; }
systemctl cat -- "$UNIT" >/dev/null 2>&1 || { say "no such unit: $UNIT"; exit 2; }
systemctl is-active --quiet "$UNIT" || { say "$UNIT is not active before the swap"; exit 2; }
BIN_R=$(readlink -f -- "$BIN"); NEW_R=$(readlink -f -- "$NEW"); CHECK_R=$(readlink -f -- "$CHECK")
NEW_SHA=$(sha "$NEW_R")
OLD_SHA=$(running_sha)   # the binary in service, which is what a rollback restores
[[ "$OLD_SHA" != none ]] || { say "cannot read the binary $UNIT is running from"; exit 2; }
[[ "$OLD_SHA" != "$NEW_SHA" ]] || { say "nothing to deploy: $NEW is the binary already in service ($OLD_SHA)"; exit 2; }
[[ "$NEW_R" != "$BIN_R" || -n "$PREV" ]] || { say "an in-place build (--new is --bin) needs --prev: without it there is no rollback"; exit 2; }
if [[ -z "$PREV" ]]; then
  [[ "$(sha "$BIN_R")" == "$OLD_SHA" ]] || { say "$BIN is not the binary in service — pass --prev with a copy of the previous binary"; exit 2; }
  PREV="$BIN_R.prev-item"; cp -p -- "$BIN_R" "$PREV"
fi
[[ -f "$PREV" ]] || { say "previous binary missing: $PREV"; exit 2; }
[[ "$(sha "$PREV")" == "$OLD_SHA" ]] || { say "$PREV is not the binary in service — refusing"; exit 2; }

restore() { # the previous binary back in service
  # The success criterion is the final readback (previous binary running, unit
  # active); these reports cover the steps that readback cannot see.
  if ! mv -f -- "$PREV" "$BIN_R"; then
    say "rollback: the previous binary could not be moved back into place ($PREV → $BIN_R)"
    return 0
  fi
  if ! systemctl restart "$UNIT" 2>&1; then
    say "rollback: the binary is back in place but the unit did not restart — run: systemctl restart $UNIT"
    return 0
  fi
}

# ---- 1. swap: copy beside the target, then rename over it ------------------------
# The running process keeps its inode; nothing it has open changes under it.
if [[ "$NEW_R" != "$BIN_R" ]]; then
  install -m 0755 -- "$NEW_R" "$BIN_R.item.tmp"
  mv -f -- "$BIN_R.item.tmp" "$BIN_R"
fi
if [[ "$(sha "$BIN_R")" != "$NEW_SHA" ]]; then
  say "the swap did not land — putting the previous binary back (nothing was restarted)"
  mv -f -- "$PREV" "$BIN_R"
  exit 1
fi

# ---- 2. restart, then the check ---------------------------------------------------
# A failed restart (unit broken, the new binary refusing to boot) must still
# roll back: it is caught here, not left to `set -e` to abort the script with
# the new binary in place and the unit down.
T0=$(date +%s)
verdict=1 reason='' detail=''
if ! systemctl restart "$UNIT" 2>&1; then
  reason="the restart did not start the unit — rolling back"
else
  if (( REHEARSE == 0 )); then
    deadline=$((T0 + WAIT))
    while :; do
      if ! systemctl is-active --quiet "$UNIT"; then reason="$UNIT is not active after the restart"; break; fi
      set +e; detail=$("$CHECK_R" "$T0" 2>&1); rc=$?; set -e
      if (( rc == 0 )); then verdict=0; reason='check passed'; break; fi
      if (( rc != 3 )); then reason="check rejected (exit $rc)"; break; fi
      if (( $(date +%s) >= deadline )); then reason="check still pending after ${WAIT}s"; break; fi
      sleep 10
    done
  else
    reason='rehearsal: rejection forced after the swap'
    sleep "$SETTLE"
  fi
fi

# ---- 3. accept, or put the previous binary back -----------------------------------
if (( verdict == 0 )); then
  # final guards at the moment of the verdict: the unit must be active AND be
  # running the new binary — a process that crashes (or never took the new
  # binary) between the check passing and now is not an acceptance
  if ! systemctl is-active --quiet "$UNIT" || [[ "$(running_sha)" != "$NEW_SHA" ]]; then
    verdict=1
    reason="the unit is not running the new binary at the moment of the verdict — treating as rejected"
  else
    common=(unit="$UNIT" reason="$reason" old_sha256="$OLD_SHA" new_sha256="$NEW_SHA"
            restart_epoch="$T0" bin="$BIN_R" rollback_binary="$PREV" check="${detail//$'\n'/ }")
    emit outcome=accepted running_sha256="$NEW_SHA" "${common[@]}"
    exit 0
  fi
fi
common=(unit="$UNIT" reason="$reason" old_sha256="$OLD_SHA" new_sha256="$NEW_SHA"
        restart_epoch="$T0" bin="$BIN_R" rollback_binary="$PREV" check="${detail//$'\n'/ }")
restore
sleep 5
active=$(systemctl is-active "$UNIT" || true)
back=$(running_sha)
if [[ "$back" == "$OLD_SHA" && "$active" == active ]]; then
  emit outcome=rolled-back running_sha256="$back" unit_state="$active" rehearsal="$REHEARSE" "${common[@]}"
  if (( REHEARSE == 1 )); then exit 0; fi
  exit 1
fi
emit outcome=ROLLBACK-FAILED running_sha256="$back" unit_state="$active" "${common[@]}"
exit 1
