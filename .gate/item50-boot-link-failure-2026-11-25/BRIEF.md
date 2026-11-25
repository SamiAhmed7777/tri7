# item 50 — BOOT-BACKFILL-LINK-FAILURE (plus: CI tor pin 16.0.1 → 16.0.3)

## What broke live

During the item-49 deploy on DNS3 (2026-11-25), the new binary exited with
code 4 twice, minutes apart, and systemd rolled both deploys back:

- 13:30:17Z, pid 2813967 (r1 deploy): `"note": "backfill: link failure at
  h2219547: prev 94f42fed… != validated 9e0d…"` → `node finish: exit 4
  (summary not persisted)` → `Main process exited, code=exited, status=4`
  → unit failed.
- 13:54:31Z, pid 2821733 (r2 deploy): same shape at
  `h2219559: prev 0d4faf0d… != validated 7d27…` → exit 4.
- Deploy driver verdict (r2): `{"outcome":"rolled-back", … "reason":"tri7d
  is not active after the restart", "old_sha256":"60ee7604…",
  "new_sha256":"3ff22133…"}` — DNS3 is back on the item-48 binary.

Root cause, not a guess — the two notes line up with the fork evidence:
the fleet was resolving a near-tied fork at h2219546/h2219558. Canonical
readbacks at 14:00:35Z (DNS2 and Pi agree exactly): h2219546 =
`9e0de473…`, h2219558 = `7d270318…`. In BOTH failing boots the SERVED
body's prev (`94f42fed…` at 13:30, `0d4faf0d…` at 13:54) was the LOSING
side's hash at the height below — i.e. the first-connected peer was still
on the losing branch, while DNS3's own persisted chain was on the eventual
canonical side. The served body was the peer's branch view, not evidence
about DNS3's chain. The OLD `backfill_fetch_bodies` doc explicitly kept
this one peer-fault class FATAL: "ONLY a kernel VALIDATION failure of a
served body stays fatal" — today was its first live occurrence.

Why fatal is the wrong policy: `systemd Restart=always` re-dials the SAME
peer on the next boot, the body phase hits the same link failure, the node
exit(4)s again — a crash loop that only ends when the PEER reorgs. A
booting node cannot let a peer's fork lag take the whole node down; the
item-17 precedent already made session errors, garbage payloads, unusable
bodies, getdata send failures and stalls non-fatal for exactly this
reason. Nothing unsound is accepted by skipping: backfill is a
boot-time convenience re-validation of ALREADY-PERSISTED headers; the
follow loop re-validates every block it actually accepts, and gap
recovery fetches anything the body phase missed.

## The change (src/node_embedded.rs)

`backfill_fetch_bodies`: the `validate_block(...)?` on a served body is
now `if let Err(e) = validate_block(...) { stats.blocks_backfill_refused
+= 1; log("backfill: served body h{height} refused ({e}) — skipping the
body phase (follow loop owns the chain)"); return Ok(validated); }` —
same shape as the other peer-fault arms. New `Stats` counter
`blocks_backfill_refused`, surfaced in the boot SUMMARY JSON next to
`blocks_validated_hist`, so operators can see skipped bodies per boot.
The function's doc comment is rewritten to record the live incident and
retire the "stays fatal" remainder.

Tests (all in the `backfill_body` family, next to the existing
peer-death/garbage/stall tests):

1. `backfill_body_link_failure_skips_the_phase_never_a_fatal_err` —
   one-shot TCP peer serves a wire-valid block whose prev is a foreign
   hash (the live 13:30/13:54 shape); asserts Ok(0),
   `blocks_received == 1`, `blocks_backfill_refused == 1`.
2. `backfill_body_kernel_refusal_links_but_fails_validation_skips_never_fatal`
   — block LINKS to our h104 but its header lies about the merkle root;
   asserts the same non-fatal skip.
3. `backfill_body_that_links_and_validates_is_counted` — control: a
   body that chains to our h104 and passes the kernel still counts
   (Ok(1), `blocks_validated_hist == 1`, refused == 0). The skip arm is
   not "always skip".

Full suite: `cargo test --lib` → **947 passed; 0 failed; 1 ignored**
(125.8 s), the 7-test `backfill_body` family green including the 3 new.

## The CI pin bump (second, independent change in the same diff)

The Windows CI job on the item-49 push (run 45620664321) failed with NO
test failures — `Invoke-WebRequest: 404 Not NotFound` fetching
`…/torbrowser/16.0.1/tor-expert-bundle-windows-x86_64-16.0.1.tar.gz`.
dist.torproject.org removed 16.0.1 (only current versions kept; 16.0.2 is
Android-only). Bumped the pin to **16.0.3**:
- digest `b70cabe2b93b5b1fd144466e42afaabe7a1a470228912657b3b5776e9cb7a9f1`
  taken from the official signed `sha256sums-signed-build.txt` for 16.0.3
  and cross-checked against a local HTTPS download of the bundle
  (byte-identical digest).
- `.github/workflows/ci.yml` `$ver/$want` + step name, and
  `deploy/windows/tor-bundle/README.md` pin table, both updated as the
  README's own bump rule requires.

## Deployment plan (after grade)

One build carries item 49 + item 50: DNS3 canary first (it has both the
archive holes the item-49 audit will repair AND the boot-exit history the
item-50 fix addresses — its boot must now survive the fork turbulence),
then Pi (on-box build), then DNS2 last with the balances/supply
acceptance check. CI Windows gate expected green again on the pin bump.

## Evidence files

- `ev_dns3_boot_exits.txt` — both failing boots' SUMMARY/note/exit lines
  + systemd reactions + the r2 rollback JSON.
- `ev_chain_at_fork_heights_dns2.txt`, `ev_chain_at_fork_heights_pi.txt`
  — canonical readbacks proving which side was canonical at the two
  heights.
- `ev_test_lib_summary.txt` — full + targeted test results.
- `item50.diff` — the exact graded bytes.
