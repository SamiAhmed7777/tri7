#!/usr/bin/env bash
# systemctl stub for the item39 protocol tests. State in $STUB_STATE/systemctl.json.
set -u
STATE="${STUB_STATE:?}/systemctl.json"
cmd="${1:-}"
[ $# -gt 0 ] && shift
case "$cmd" in
  is-active)
    [ "$(jq -r '.active' "$STATE")" = "true" ] && exit 0 || exit 3 ;;
  show)
    # args: <unit> -p MainPID --value
    prop=""; valonly=0
    while [ $# -gt 0 ]; do
      case "$1" in
        -p) prop="$2"; shift 2 ;;
        --value) valonly=1; shift ;;
        *) shift ;;
      esac
    done
    if [ "$prop" = "MainPID" ] && [ "$valonly" = "1" ]; then
      jq -r '.mainpid' "$STATE"
    fi
    exit 0 ;;
  restart)
    n="$(cat "${STUB_STATE}/restart_count" 2>/dev/null || echo 0)"; n=$((n+1))
    echo "$n" > "${STUB_STATE}/restart_count"
    if [ -f "${STUB_STATE}/restart_fail_at" ] && [ "$n" -ge "$(cat "${STUB_STATE}/restart_fail_at")" ]; then
      echo "systemctl-stub: restart failed (controlled failure)" >&2
      exit 1
    fi
    oldpid="$(jq -r '.mainpid' "$STATE")"
    exe="$(jq -r '.exe' "$STATE")"
    # alt_exe_after: use alt_exe once restart_count exceeds it (models a unit
    # definition whose ExecStart resolves elsewhere — running-inode mismatch)
    if [ -f "${STUB_STATE}/alt_exe_after" ] && [ "$n" -gt "$(cat "${STUB_STATE}/alt_exe_after")" ]; then
      alt="$(jq -r '.alt_exe // ""' "$STATE")"
      [ -n "$alt" ] && [ "$alt" != "null" ] && exe="$alt"
    fi
    if [ -n "$oldpid" ] && [ "$oldpid" != "0" ] && [ "$oldpid" != "null" ]; then
      kill "$oldpid" 2>/dev/null || true
    fi
    setsid nohup "$exe" >/dev/null 2>&1 < /dev/null &
    newpid=$!
    jq --argjson p "$newpid" '.mainpid = $p' "$STATE" > "${STATE}.tmp" && mv "${STATE}.tmp" "$STATE"
    exit 0 ;;
  *)
    echo "systemctl-stub: unsupported: $cmd $*" >&2
    exit 1 ;;
esac
