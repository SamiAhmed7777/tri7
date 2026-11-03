#!/usr/bin/env bash
# capture_phase6_evidence.sh — self-contained evidence capture for the
# tri7d phase-6 network-primary remediation. Run as root on DNS2 AFTER the
# deploy. Writes tri7d_phase6_evidence.txt with verbatim transcripts and
# explicit rc= lines for every command. No mutation: read-only probes only.
set -uo pipefail

OUT="${1:-/tmp/tri7_phase6_evidence.txt}"
CREDS="trianglesrpc:86f228fda3a6c3bf829fd47c34b50956f15e7889c96bbfa4c4d90250e8fda8de"
RPC="http://127.0.0.1:19112/"
R=""

hdr() { printf '\n===== %s =====\n' "$1" >> "$OUT"; }
cmd() { # cmd "<label>" <command...>
  local label="$1"; shift
  printf '\n### CMD: %s\n$ ' "$label" >> "$OUT"
  printf '%s\n' "$*" >> "$OUT"
  local rc=0
  "$@" >> "$OUT" 2>&1 || rc=$?
  printf 'rc=%d\n' "$rc" >> "$OUT"
  R=$rc
}
cmdsh() { # cmdsh "<label>" "<shell string>"  (for pipes)
  local label="$1"; shift
  printf '\n### CMD: %s\n$ %s\n' "$label" "$1" >> "$OUT"
  local rc=0
  bash -c "$1" >> "$OUT" 2>&1 || rc=$?
  printf 'rc=%d\n' "$rc" >> "$OUT"
  R=$rc
}

: > "$OUT"
printf '# tri7d phase-6 network-primary remediation — live evidence\n' >> "$OUT"
printf '# captured: %s UTC on %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$(hostname)" >> "$OUT"

hdr "0. deployed binary identity"
cmdsh "sha256 repo build vs deployed" 'sha256sum /root/tri7/target/release/follow_tip /opt/tri7/bin/follow_tip'
cmd "systemd active" systemctl is-active tri7d.service
cmdsh "deployed binary sha256 (short)" 'sha256sum /opt/tri7/bin/follow_tip | cut -c1-16'

hdr "1. listeners (P2P inbound 24113 + RPC 19112)"
cmdsh "ss -tlnp tri7d ports" "ss -tlnp | grep -E '24113|19112|19113'"
cmdsh "ufw 24113 rule" 'ufw status | grep 2411'

hdr "2. legacy trianglesd retired"
cmd "trianglesd state" systemctl is-active trianglesd.service
cmd "trianglesd enabled-state" systemctl is-enabled trianglesd.service

hdr "3. chain advancement restored (h2204067 -> h2204068)"
cmdsh "getblockcount (native, source tag)" "curl -s -m 5 -u $CREDS --data '{\"jsonrpc\":\"1.0\",\"id\":\"v\",\"method\":\"getblockcount\",\"params\":[]}' $RPC"
cmdsh "getbestblockhash (native)" "curl -s -m 5 -u $CREDS --data '{\"jsonrpc\":\"1.0\",\"id\":\"v\",\"method\":\"getbestblockhash\",\"params\":[]}' $RPC"
cmdsh "getblock NEW tip h2204068 (above disk store tip -> recent cache)" "curl -s -m 5 -u $CREDS --data '{\"jsonrpc\":\"1.0\",\"id\":\"v\",\"method\":\"getblock\",\"params\":[\"0a5b2288894c4c2bda2c0dcd85d42483c0a4498f386fa535b64977182b12764c\",false]}' $RPC | python3 -c 'import json,sys; d=json.load(sys.stdin)[\"result\"]; print({k: d[k] for k in (\"height\",\"time\",\"bits\",\"confirmations\")})'"
cmdsh "journal: mint push -> TIP ADVANCE -> relay inv" 'journalctl -u tri7d --no-pager --since "2026-11-02 15:30" | grep -E "TIP ADVANCE|relay: announced|checksum oracle" | tail -4'
cmdsh "stake_mint acceptance (LIVE ACCEPT +2.0s)" 'grep -E "LIVE ACCEPT|p2p submit" /root/tri7/logs/stake_live_c4.log | tail -2'

hdr "4. relay serving over inbound P2P (getdata: recent-cache tip + disk-store block)"
cmdsh "relay_verify.py transcript" 'python3 /root/.hermes/cache/scratch/relay_verify.py 0a5b2288894c4c2bda2c0dcd85d42483c0a4498f386fa535b64977182b12764c fab257170c06c1a824c6f6a0a5f01a38ea7313a1b90eaf27bebc3acd9c3d30f3'

hdr "5. healthcheck gating"
cmdsh "status JSON (key fields)" '/opt/tri7/bin/follow_tip --status --state-dir /var/lib/tri7 | python3 -c "import json,sys; d=json.load(sys.stdin); print({k: d[k] for k in (\"height\",\"peers\",\"last_peer_utc\",\"seconds_since_peer\",\"seconds_since_advance\",\"degraded\",\"stale\",\"listen_port\",\"static_mode\")})"'
cmdsh "degraded gate fires (short thresholds -> rc 3)" '/opt/tri7/bin/follow_tip --status --state-dir /var/lib/tri7 --peer-quiet-secs 5 --advance-quiet-secs 10 >/dev/null 2>&1; echo "status exit=$?"'
cmdsh "health unit ExecStart (gate semantics)" 'grep ExecStart /etc/systemd/system/tri7d-health.service'

hdr "6. test suite (final build)"
cmdsh "lib + follow_tip unit tests" 'cd /root/tri7 && source $HOME/.cargo/env && cargo test --release --lib --bin follow_tip 2>&1 | grep "test result"'

hdr "7. peer attach rotation (fleet dark at capture time — all targets tried)"
cmdsh "latest rotation burst" 'journalctl -u tri7d --no-pager --since "-4 min" | grep -E "connect|no outbound" | tail -7'

printf '\n# capture rc of last command: %d\n' "$R" >> "$OUT"
echo "evidence written to $OUT"
