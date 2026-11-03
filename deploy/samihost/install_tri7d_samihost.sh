#!/usr/bin/env bash
# install_tri7d_samihost.sh — idempotent installer for follow-mode tri7d on
# Samihost (194.163.161.162). Run as root; artifacts land in /tmp/tri7-ship.
# NO STAKING: follow_tip cannot mint; stake_mint is NOT shipped; conf staking=0.
# Does NOT touch the legacy triangles client (removal is a separate approved step).
set -euo pipefail
SHIP=/tmp/tri7-ship

echo "== verify shipped artifacts =="
cd "$SHIP"
sha256sum -c manifest.sha256

echo "== system user + dirs =="
id tri7 &>/dev/null || useradd --system --home-dir /var/lib/tri7 --shell /usr/sbin/nologin tri7
install -d -m 0755 /opt/tri7/bin
install -d -m 0750 -o root -g tri7 /etc/tri7
install -d -m 0700 -o tri7 -g tri7 /var/lib/tri7

echo "== binaries (root-owned, world-executable) =="
install -m 0755 -o root -g root "$SHIP/follow_tip"       /opt/tri7/bin/follow_tip
install -m 0755 -o root -g root "$SHIP/read_wallet"      /opt/tri7/bin/read_wallet
install -m 0755 -o root -g root "$SHIP/tri7_wallet_gui"  /opt/tri7/bin/tri7_wallet_gui

echo "== conf with fresh RPC creds (generated on this host; never DNS2's) =="
if [[ ! -f /etc/tri7/triangles.conf ]]; then
  RPCPASS=$(head -c 32 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | head -c 32)
  umask 027
  cat > /etc/tri7/triangles.conf <<CONF
# tri7d (follow-mode follower) on Samihost — installed $(date -u +%Y-%m-%dT%H:%M:%SZ)
# Only rpcuser/rpcpassword are parsed by follow_tip; the staking lines below
# are declarative intent: this host must NEVER stake (wallets here are kept
# cold; any staking decision belongs to the operator, not this deploy).
rpcuser=tri7rpc
rpcpassword=$RPCPASS
rpcport=19112
rpcallowip=127.0.0.1
staking=0
forcestaking=0
CONF
  chown root:tri7 /etc/tri7/triangles.conf
  chmod 0640 /etc/tri7/triangles.conf
  umask 022
else
  echo "  conf exists — keeping (idempotent re-run)"
fi

echo "== state seed =="
if [[ ! -f /var/lib/tri7/tipstate.json ]]; then
  install -m 0600 -o tri7 -g tri7 "$SHIP/tipstate-seed.json" /var/lib/tri7/tipstate.json
else
  echo "  tipstate exists — keeping"
fi
install -m 0640 -o root -g tri7 "$SHIP/balances.json"     /var/lib/tri7/balances.json
install -m 0640 -o root -g tri7 "$SHIP/wallet_utxos.json" /var/lib/tri7/wallet_utxos.json

echo "== blocks store (frame-complete snapshot of DNS2 chain) =="
if [[ ! -f /var/lib/tri7/blk0001.dat ]]; then
  install -m 0644 -o tri7 -g tri7 "$SHIP/blk0001.dat" /var/lib/tri7/blk0001.dat
else
  echo "  blk0001.dat exists — keeping"
fi
grep -q 'blk0001.dat' "$SHIP/manifest.sha256" && \
  cp "$SHIP/manifest.sha256" /var/lib/tri7/BLK_SNAPSHOT.sha256 && \
  chown tri7:tri7 /var/lib/tri7/BLK_SNAPSHOT.sha256

echo "== systemd units =="
install -m 0644 "$SHIP/tri7d.service"        /etc/systemd/system/tri7d.service
install -m 0644 "$SHIP/tri7d-health.service" /etc/systemd/system/tri7d-health.service
install -m 0644 "$SHIP/tri7d-health.timer"   /etc/systemd/system/tri7d-health.timer
systemd-analyze verify /etc/systemd/system/tri7d.service \
  /etc/systemd/system/tri7d-health.service /etc/systemd/system/tri7d-health.timer
systemctl daemon-reload

echo "== install complete =="
echo "Start with:  systemctl enable --now tri7d.service tri7d-health.timer"
echo "RPC check:   curl -s -u tri7rpc:<pass> -d '{\"method\":\"getblockcount\"}' http://127.0.0.1:19112"
echo "RPC creds:   grep rpcpassword /etc/tri7/triangles.conf  (root:tri7 0640)"
