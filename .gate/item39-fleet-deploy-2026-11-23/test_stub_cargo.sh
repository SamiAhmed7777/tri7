#!/usr/bin/env bash
# cargo stub for the item39 protocol tests. Fails if $STUB_STATE/cargo_fail exists;
# otherwise installs the fake new binary at <manifest-dir>/target/release/follow_tip.
set -u
manifest=""
while [ $# -gt 0 ]; do
  case "$1" in
    --manifest-path) manifest="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$manifest" ] || { echo "cargo-stub: no --manifest-path given" >&2; exit 1; }
if [ -f "${STUB_STATE:?}/cargo_fail" ]; then
  echo "cargo-stub: controlled build failure" >&2
  exit 1
fi
d="$(dirname "$manifest")"
mkdir -p "$d/target/release"
# model cargo's real write discipline (temp file + rename, never O_TRUNC over a running
# executable — rename over a busy path is legal, cp -style truncate hits ETXTBSY)
cp "${STUB_STATE}/newbin" "$d/target/release/.follow_tip.new.$$"
mv -f "$d/target/release/.follow_tip.new.$$" "$d/target/release/follow_tip"
chmod 0755 "$d/target/release/follow_tip"
echo "cargo-stub: stub build completed (fake follow_tip installed)"
exit 0
