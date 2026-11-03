#!/usr/bin/env bash
# install_tri7d.sh — idempotent phase-5a installer for tri7d on DNS2.
# Run as root from the repo root (/root/tri7). Safe to re-run.
# POST-CUTOVER (2026-11-02): the unit installs with --rpc-port 19112 --no-proxy
# (tri7d owns the standard port; legacy trianglesd retired). See
# deploy/tri7d_rpc_cutover.md.
#
# FRESH INSTALLS: use deploy/tri7d_fresh_install.sh instead — one command that
# takes a bare machine to a fully-synced node with a NEW wallet via
# follow_tip --sync-from-zero (see deploy/FROM_ZERO_RUNBOOK.md). THIS script
# is for the DNS2 primary, which was seeded from the legacy daemon's archive.
set -euo pipefail

REPO_BIN=./target/release/follow_tip

# 1. system user (no shell, home = state dir)
id tri7 &>/dev/null || useradd --system --home-dir /var/lib/tri7 --shell /usr/sbin/nologin tri7

# 2. directories
install -d -m 0755 /opt/tri7/bin
install -d -m 0750 -o root -g tri7 /etc/tri7
install -d -m 0700 -o tri7 -g tri7 /var/lib/tri7

# 3. binary (root-owned, world-executable)
install -m 0755 -o root -g root "$REPO_BIN" /opt/tri7/bin/follow_tip

# 4. conf copy with RPC creds — root:tri7 0640, never /root/.triangles itself
install -m 0640 -o root -g tri7 /root/.triangles/triangles.conf /etc/tri7/triangles.conf

# 5. seed state (only if absent — never clobber a live tipstate)
if [[ ! -f /var/lib/tri7/tipstate.json ]]; then
  install -m 0600 -o tri7 -g tri7 /root/tri7/tipstate.json /var/lib/tri7/tipstate.json
fi

# 5b. phase-5b RPC surface inputs:
#  - replay snapshots (balances.json / wallet_utxos.json) — refresh these by
#    re-running the balances bin and re-copying when the replay is redone
install -m 0640 -o root -g tri7 /root/tri7/balances.json /var/lib/tri7/balances.json
install -m 0640 -o root -g tri7 /root/tri7/wallet_utxos.json /var/lib/tri7/wallet_utxos.json
#  - blocks file for the native block/tx index: HARDLINK (same inode, zero
#    disk cost, follows daemon appends; blk0001.dat is 0605 so tri7 can read
#    the inode). Re-run this line if the daemon ever recreates the file.
ln -f /root/.triangles/blk0001.dat /var/lib/tri7/blk0001.dat

# 6. units (daemon + health service/timer)
install -m 0644 /root/tri7/deploy/tri7d.service /etc/systemd/system/tri7d.service
install -m 0644 /root/tri7/deploy/tri7d-health.service /etc/systemd/system/tri7d-health.service
install -m 0644 /root/tri7/deploy/tri7d-health.timer /etc/systemd/system/tri7d-health.timer

# 7. verify + (re)load
systemd-analyze verify /etc/systemd/system/tri7d.service \
  /etc/systemd/system/tri7d-health.service /etc/systemd/system/tri7d-health.timer
systemctl daemon-reload

echo "install complete. Enable with:"
echo "  systemctl enable --now tri7d.service tri7d-health.timer"
echo "Healthcheck: /opt/tri7/bin/follow_tip --status --state-dir /var/lib/tri7"
