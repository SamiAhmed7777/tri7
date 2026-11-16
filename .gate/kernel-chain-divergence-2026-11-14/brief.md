# Item 25 — KERNEL-CHAIN-DIVERGENCE: contain + detect + heal

## Context (the live incident this fixes)

DNS3, 2026-11-14 05:41–06:56Z: the node's persisted stake-modifier chain
diverged from canonical at a generated-modifier height (h2211475). The boot
path loads modifiers VERBATIM; the load-time checksum chain could NOT catch
the class because the poison was computed in-session — every checksum above
the divergence was rebuilt from the wrong modifier (stepwise-cumulative,
internally consistent). The validator then hard-rejected every inbound block
whose kernel modifier walk reached the diverged region, and that rejection
EXITED the daemon (EXIT_VALIDATE=3) → ~50-boot crash-loop wedge. Stored
proofs/hashes/times were byte-identical fleet-wide; ONLY the generated
modifier values differed. Healed that morning by manual surgery (tipstate
transplant from DNS2); this commit makes the daemon do it itself.

## The fix (three parts, matching workqueue item 25)

### (a) CONTAIN — a received-block validation failure never exits
`handle_message`'s live-block path (the only path that previously turned a
peer block failure into `finish(EXIT_VALIDATE)` via the caller's
`outbound-is-fatal` branch):
- `validate_block` Err → log CONTAINED + drop the peer + `return Ok(())`
  (was `return Err(e)`).
- `advance_with_block` Err (stake-modifier checksum divergence / modifier
  computation / oracle readback) → same containment.
- The caller's `outbound-is-fatal → finish(EXIT_VALIDATE)` branch is now
  unreachable from block validation. Kernel-state failures additionally run
  (b)+(c) and re-inject the cached payload so the (likely canonical) block
  applies after the repair through the normal reinject path.
- The from-zero sync's served-body kernel failure stays fatal — unchanged
  disclosed remainder from the item-17 tick (no live occurrence; the
  assume-valid envelope is the primary guard there).

### (b) DETECT — exact replay scan + sampled kernel audit
- `scan_modifier_chain`: forward replay of the modifier chain over the
  loaded window using the same `compute_next_stake_modifier_era` the
  in-session advance uses. Below `replay_floor` (selection lookback
  truncated by the window) stored values are trusted, never compared —
  no false mismatches. `first_bad` is exactly the divergence point (above
  it the poison is self-consistent and reproduces).
- `audit_kernel_samples` (the item's K-sampled check, K=32): recompute
  sampled kernels from the RECORDS range via `recompute_stake_kernel`
  (new public helper in kernel.rs — the exact guards + walk + hash of
  `check_proof_of_stake` without needing a parsed Block, because BlockRec
  carries spends without tx attribution) and compare with the stored
  proofs (byte-identical fleet-wide — verified live). Any resolvable
  spend of the block is an eligible candidate; ANY candidate reproducing
  the stored proof passes. Unresolvable inputs skip, never a false
  mismatch.

### (c) HEAL — re-seed the window in place
`rebase_modifier_chain`: from the first recomputable generated height,
replay the canonical computation IN PLACE (each entry reads its
already-re-seeded predecessors — computing over a pre-rewrite snapshot
would rebuild the poison verbatim; that exact trap is covered by the
tests) and rebuild the stepwise-cumulative checksum chain over the
rewritten span. {hash, prev_hash, time, bits, pos, entropy, proof} stay
VERBATIM — the incident class is a generated-modifier divergence only.
`detect_and_heal_modifier_chain` wraps scan → rebase → re-verify (replay
clean + fresh samples reproduce the stored proofs); verification failure
keeps the window loud and the contained posture — fail-honest, no exit.
Hooks: at boot (after `boot_backfill_kernel_window`, before any validation)
and on every contained kernel-state failure. A clean window is a verified
no-op (the DNS2/Pi unregressed shape).

## Tests (+6, all in src/node_embedded.rs tests, fixture with REAL kernels)

- `item25_healthy_window_scan_and_sample_audit_are_clean` — canonical
  fixture replays clean, samples reproduce, detect is a no-op.
- `item25_self_consistent_divergence_detected_healed_and_verified` —
  the propagated self-consistent poison (checksum gate PASSES — asserted
  by construction) is detected at exactly the poisoned height, healed in
  place to byte-equality with canonical (modifiers, generated flags,
  checksums), evidence fields untouched, samples clean again.
- `item25_divergence_below_the_replay_floor_is_fail_honest` — below-floor
  divergence: scan clean, audit catches it, heal refuses (fail-honest,
  poisoned value untouched).
- `item25_kernel_state_failure_classifier` — the wedge error strings vs
  transport/garbage negatives.
- `item25_inbound_kernel_failure_contained_healed_and_block_applies` —
  THE INCIDENT END TO END: poisoned window + canonical relayed block over
  an OUTBOUND loopback peer → handle_message returns Ok (never a fatal
  error), peer dropped, tip unchanged, heal fired, cached payload
  re-injected; second pass (the pump's reinject drain) applies the block —
  tip advances. The DNS3 crash-loop shape, fixed.
- `item25_kernel_recompute_parity_with_the_validator` — the audit helper
  hashes exactly what `check_proof_of_stake` hashes for the same
  (chain, input, block_time).

Fixture notes: 60 s block spacing (= TARGET_SPACING) so the live-era
difficulty EMA holds the walk's own bits; a dense insert-only coin pool so
the era-limit-target solver is certain; the poison helper propagates the
flipped modifier upward via the same in-session computation the live
incident used.

## Honest scope notes

- The below-floor class is detected but not healable in place (selection
  lookback truncated by the window) — loud + contained, no exit; recovery
  remains the peer-window re-seed (live-surgery) lane.
- The from-zero sync path's served-body kernel failure remains fatal
  (pre-existing disclosed remainder, unchanged).
- The heal's oracle is the deterministic consensus replay over the window's
  canonical stored evidence (proofs/hashes/times — byte-identical
  fleet-wide, verified live in the DNS3 surgery). The rpc_seed_window
  machinery stays for the from-zero path; the follower's rpc client points
  at itself in the current topology, so a peer-RPC re-seed lane is not
  wired here.
- Deploys: follow-tip binary fleet-wide; DNS3's current tipstate is
  post-surgery canonical, so the boot audit must be a verified no-op on
  all three nodes.
