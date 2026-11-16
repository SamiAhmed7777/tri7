# Seed purge — Triangles is NEVER on public clearnet (operator directive)

Operator policy (Sami, 2026-11-13, verbatim): "Triangles should not be on
clearnet." Implemented at the code level, not just firewalled.

## Changes (src/node_embedded.rs, src/gui/node_supervisor.rs)

1. `DEFAULT_PEER_SEEDS` (the compiled bootstrap seed floor in every wallet
   and node): the two cleartext entries — `46.62.249.20:443` (Pi public
   443) and `100.85.236.10:24113` (samihost) — are REMOVED. The floor is now
   overlay-only: DNS2's tor onion + DNS2's i2p tunnel. Fresh wallets
   bootstrap through their tor/i2p proxies (the supervisor owns those), never
   a public listener.
2. `DEFAULT_PEER` / `FALLBACK_PEER` (the GUI supervisor's bootstrap constants,
   previously the same two clearnet addresses, passed to ensure_running as
   spawned-node `--daemon` args): now ALIASED to FLEET_ONION_SEED /
   FLEET_I2P_SEED (single source of truth, dual overlay networks).
3. Policy regression test added inside the existing augment test: fails if
   any seed entry is tagged `clearnet` or carries a clearnet-looking spec
   (46.62.* / 100.*), so the floor cannot silently regress.

## Why this is behavior-changing but safe

- The seed floor is a FLOOR (appended after configured peers, deduped) —
  removing entries shrinks bootstrap options to the two overlay networks,
  which the operator policy requires. Conf addnode (onion/i2p only — the
  parser rejects cleartext IPs, verified live on the laptop today) and
  operator CLI args still take precedence.
- Listeners were already loopback-only fleet-wide at the operator's
  direction; nftables tri7_p2p_policy (DNS2 + Pi) drops 24113/443 from
  non-lo/non-tailscale interfaces. The dialer now matches the listener side.
- Live verification before this commit: Pi rebuilt from this tree, restarted,
  booted NON-headless, synced to fleet tip h2211329 over onion + i2p only;
  DNS2 minting at the tip; laptop (old binary) still reaches the fleet via
  its compiled seeds until it receives this build (deploy after CI).

## Verification

cargo test --lib: 775 passed / 0 failed / 1 ignored (the augment test now
carries the policy assertions); GUI featureful check + windows-gnu release
--bins green — gates.log = exactly one final-tree run.

FILES: src/node_embedded.rs, src/gui/node_supervisor.rs
