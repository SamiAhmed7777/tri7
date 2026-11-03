#!/usr/bin/env bash
# tri7d_fresh_install.sh — ONE-COMMAND fresh install of a from-zero tri7d node.
#
# Takes a BARE machine (no chain data, no state, no wallet) to a fully-synced,
# staking-capable node with a BRAND-NEW wallet, in one command:
#
#   sudo ./tri7d_fresh_install.sh --peer HOST:PORT [--peer HOST:PORT ...]
#        [--wallet PATH] [--stake] [--listen|--no-listen]
#
# The heavy lifting is follow_tip itself:
#   --sync-from-zero  fetches block 0, pins it to the compiled-in genesis
#                     hash, then walks the ENTIRE chain over P2P getheaders +
#                     batched getdata, validating EVERY block through the
#                     normal acceptance gate (Hash9 + link + merkle + mtp +
#                     nBits EMA + PoW target / PoS kernel + block sig +
#                     finality). Progress persists every 1000 blocks and on
#                     SIGINT/SIGTERM — the sync is RESUMABLE across restarts.
#   --new-wallet      creates a fresh SQLite TRIw wallet with freshly
#                     generated keys (never copied from another node).
#
# Idempotent: re-running keeps existing state/wallet and just (re)installs
# the binary + units.
set -euo pipefail

BIN_SRC="${BIN_SRC:-./target/release/follow_tip}"
STATE_DIR="${STATE_DIR:-/var/lib/tri7}"
CONF_DIR="${CONF_DIR:-/etc/tri7}"
BIN_DST="${BIN_DST:-/opt/tri7/bin/follow_tip}"
WALLET="${WALLET:-$STATE_DIR/wallet.dat}"
STAKE=0
LISTEN=1
PEERS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --peer) PEERS+=("$2"); shift 2 ;;
    --wallet) WALLET="$2"; shift 2 ;;
    --stake) STAKE=1; shift ;;
    --listen) LISTEN=1; shift ;;
    --no-listen) LISTEN=0; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [[ ${#PEERS[@]} -eq 0 ]]; then
  echo "ERROR: at least one --peer HOST:PORT is required (from-zero needs a live peer)" >&2
  exit 2
fi
if [[ ! -x "$BIN_SRC" ]]; then
  echo "ERROR: $BIN_SRC missing — build first: cargo build --release --bin follow_tip" >&2
  exit 2
fi

# 1. system user + dirs
id tri7 &>/dev/null || useradd --system --home-dir "$STATE_DIR" --shell /usr/sbin/nologin tri7
install -d -m 0755 "$(dirname "$BIN_DST")"
install -d -m 0750 -o root -g tri7 "$CONF_DIR"
install -d -m 0700 -o tri7 -g tri7 "$STATE_DIR"

# 2. binary
install -m 0755 -o root -g root "$BIN_SRC" "$BIN_DST"

# 3. conf (generate RPC creds on a bare machine; keep an existing copy)
if [[ ! -f "$CONF_DIR/triangles.conf" ]]; then
  RPCUSER="tri7rpc"
  RPCPASS="$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | head -c 24)"
  [[ -n "$RPCPASS" ]] || RPCPASS="$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')"
  install -m 0640 -o root -g tri7 <(printf 'rpcuser=%s\nrpcpassword=%s\nserver=1\n' "$RPCUSER" "$RPCPASS") \
    "$CONF_DIR/triangles.conf"
  echo "generated $CONF_DIR/triangles.conf (fresh RPC creds)"
fi

# 4. units
PEER_FLAGS=""
for p in "${PEERS[@]}"; do PEER_FLAGS+="--daemon ${p} "; done
STAKE_FLAGS=""; [[ $STAKE -eq 1 ]] && STAKE_FLAGS="--stake "
LISTEN_FLAGS="--no-listen"; [[ $LISTEN -eq 1 ]] && LISTEN_FLAGS="--listen-port 24113 --listen-bind 0.0.0.0"
cat > /etc/systemd/system/tri7d.service <<EOF
[Unit]
Description=tri7 daemon (from-zero synced node)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=tri7
Group=tri7
ExecStart=$BIN_DST $PEER_FLAGS--sync-from-zero --sync-workers 4 --new-wallet $WALLET $STAKE_FLAGS$LISTEN_FLAGS --conf $CONF_DIR/triangles.conf --state-dir $STATE_DIR --forever --rpc-port 19112 --rpc-bind 127.0.0.1 --no-proxy --blocks-file $STATE_DIR/blk0001.dat --balances $STATE_DIR/balances.json --wallet-utxos $STATE_DIR/wallet_utxos.json --index-dir $STATE_DIR/index
Restart=on-failure
RestartSec=30
MemoryMax=3G
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=$STATE_DIR
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
EOF

# healthcheck unit (tip freshness gate) + timer
cat > /etc/systemd/system/tri7d-health.service <<'EOF'
[Unit]
Description=tri7d healthcheck
[Service]
Type=oneshot
ExecStart=/opt/tri7/bin/follow_tip --status --state-dir /var/lib/tri7 --max-write-age 900
EOF
cat > /etc/systemd/system/tri7d-health.timer <<'EOF'
[Unit]
Description=tri7d healthcheck timer
[Timer]
OnBootSec=10min
OnUnitActiveSec=5min
[Install]
WantedBy=timers.target
EOF
systemd-analyze verify /etc/systemd/system/tri7d.service 2>/dev/null || true
systemctl daemon-reload

# 5. enable + start (the from-zero sync runs inside the unit; it resumes
# automatically after any restart)
systemctl enable --now tri7d.service tri7d-health.timer

echo
echo "fresh install complete:"
echo "  status : $BIN_DST --status --state-dir $STATE_DIR"
echo "  logs   : journalctl -u tri7d -f   (look for 'from-zero: h<NNNN> ... blk/s, ETA')"
echo "  rpc    : curl -s -u \$(grep rpcuser $CONF_DIR/triangles.conf | cut -d= -f2):\\\$(grep rpcpassword $CONF_DIR/triangles.conf | cut -d= -f2) \\"
echo "           -d '{\"jsonrpc\":\"1.0\",\"id\":0,\"method\":\"getblockcount\",\"params\":[]}' http://127.0.0.1:19112/"
echo "  wallet : $WALLET (brand-new keys; visible via read_wallet)"
echo "  resume : systemctl restart tri7d  (sync continues from the persisted tip)"
