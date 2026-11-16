# tri7 fleet deploy manifest — item 28 + 29 (commits 260ed6f, 1075100)

Kind: deploy (binary-only cutover of three live staking nodes; no config, no unit, no data changes)

## What ships
- DNS2 + DNS3 (x86_64): /root/tri7/target/release/follow_tip, sha256 prefix f2e3d3aa03980094, built clean from master 1075100 (release profile, 2m12s, markers verified: all 4 new log strings present).
- Pi (aarch64): git pull to 1075100 + native `cargo build --release` on the Pi (its own cargo 1.99.1; its unit ExecStarts /root/tri7/target/release/follow_tip directly).

## Procedure (per host)
1. Record current exe sha256 + tip height BEFORE.
2. Backup current binary (same dir, .bak-<epoch>).
3. Install new binary (preserve 0755 root:root; atomic mv).
4. systemctl restart tri7d.
5. Verify: unit active, journal shows clean boot (no validation-failure exit), RPC tip live and advancing, connections >= 1, NRestarts unchanged, new exe hash in place, no impairment/stale lines beyond the documented boot-window signatures.

## Known-expected boot signatures (NOT failures)
- Pi fresh boot: warmup gates verack ~5-6 min ("no outbound target reachable — starting HEADLESS" is expected), then self-resolves; boot audit + backfill lines.
- A raced-mint stale-live-view impairment right after a restart is healed by item-27 autoheal in-process.

## Rollback
- Per host: restore the .bak binary + restart. No data/config changes to roll back.

## Risk assessment
- The parity loop cron observes the fleet and treats restarts as normal ops (restart lever #6 was used 8h ago on the Pi).
- Item 28 (this deploy) makes the adopt-fail-remint wedge self-healing; item 29 makes stop/restart clean-exit instead of SIGKILL — this deploy REDUCES operational risk going forward.
- DNS2 first (canary, local, full observation), then DNS3, then Pi (build ~5-10 min on Pi hardware).
