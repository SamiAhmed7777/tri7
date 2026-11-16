# BRIEF — item 21 INBOUND-RECON (tri7 follow-loop, src/node_embedded.rs)

## Problem (live fleet catch 2026-11-11 05:1xZ, healed same tick by a manual conf bridge)

A tri7 daemon reconciles ONLY against OUTBOUND peers. Tips travel only inside
getheaders replies, and the 30 s poll tick sent the tip locator to
`peers.iter_mut().filter(|p| p.outbound)` — a peer that dialed IN was polled by
NOBODY. Its branch was invisible to our judge FOREVER:

- the empty-reply evidence paths (item 18 height fast path, item 19 streak)
  never saw its replies (it was never asked anything);
- its handshake `start_height` is frozen at dial time, so even the trigger
  inputs we do keep per peer could not see its fork growth (live: the PC
  dialed into DNS2 at an equal height, then solo-minted a ~1.42x-heavier
  branch; DNS2 kept minting a doomed lighter branch for ~40 min until a
  MANUAL i2p addnode bridge gave DNS2 an outbound dial to the PC —
  urn:ump:y4blr2yfe33r34ytgwsgizuh4pnkq43eue4avetubrpxmob2hjrq).

## Fix (this diff, all in src/node_embedded.rs)

1. **Poll inbound peers** (`if !peers.is_empty()` + `for p in peers.iter_mut()`).
   The tip locator now goes to every live peer; new `Stats::inbound_polls`
   counter (rendered in stats_json) counts the inbound half. Replies re-enter
   the SAME headers handler, so item-17b recon judging, item-18/19 evidence
   paths and the window-locator re-ask all apply to inbound peers unchanged.
2. **Per-poll claimed-tip tracking** (`Peer.claimed_tip: Option<(u64, Hash)>`):
   - a batch that CHAINS onto our tip claims its last served header
     (`claim_from_extending_batch`: tip + len at the batch's last hash);
   - a recon batch's claim is recorded by the judge itself
     (`ReconAcc.judged_claim`, set where their_h/their_tip are computed, moved
     onto the Peer by `recon_dispatch` for Ignored/Switched, and computed from
     the running accumulator for Extended — a judged-lighter branch that keeps
     growing keeps its claim alive instead of being forgotten by the closed
     reconciliation);
   - exposed in getpeerinfo (`claimed_tip_height`, `claimed_tip_hash`) as the
     ops-diagnosis surface (PeerRec + peer_table_json + refresh site).
3. **Claim-divergence evidence path** (`claim_divergence_verdict` + third
   trigger in `empty_reply_fork_recon_due`): on an empty reply, the peer's
   claim is compared against our chain — DIVERGENT when the claim is a taller
   tip whose hash we neither index nor queue as pending (ahead-on-our-chain
   shape excluded so healthy body fetches never false-fire), or a different
   hash at a height we hold. Divergence fires the window-locator re-ask on ONE
   empty reply (same 60 s cooldown + awaiting_window_reply echo guards as the
   other two triggers), reason tag `claim-divergence` in the fire log; the log
   line now prints the claimed height (falling back to start_height).
4. **Adopted-branch body fetch reaches the branch's owner** (the dispatch site
   for `pending_from_headers` getdata): outbound peer preferred, ANY live peer
   (incl. inbound) as fallback. Previously the fetch went to the FIRST OUTBOUND
   peer unconditionally — after adopting an inbound peer's heavier branch our
   outbound set may hold NONE of those bodies, leaving the tip rolled back with
   unfetchable pending headers.

## Regression tests (lib suite)

- `inbound_claim_divergence_triggers_recon_and_adopts_the_heavier_branch` —
  THE item-21 bar: equal-at-handshake start_height (the item-18 height path
  provably cannot fire), streak 0, claim = taller tip with an unknown hash →
  trigger fires → `recon_intake` adopts the heavier branch (Switched, fork 102,
  4 blocks, reorgs 1, pending queued at absolute heights) and the judged claim
  is recorded for continued tracking.
- `claim_divergence_verdict_shapes` — verdict truth table: taller-unknown →
  true; taller-but-pending-queued → false; taller-but-hash-is-our-chain →
  false; equal-height same hash → false; equal-height different hash → true;
  mid-window different hash → true; below-window → None; no claim → None.
- `extending_batch_claim_records_the_served_tip` — a chain-extending batch
  claims tip+len at its last header, and that claim flips from divergent to
  non-divergent once the hashes are queued (bodies in flight).
- `body_source_choice_prefers_the_recon_advertiser_over_healthy_outbound` —
  THE body-fetch blocker pinned: the inbound advertiser is chosen even when a
  healthy outbound peer is connected; advertiser-gone → outbound fallback; no
  recon source → outbound; inbound-only set → sole peer; nobody live → None.
- `recon_dispatch_transfers_claim_and_source_to_the_peer` — the WIRING test
  through `recon_dispatch` (the exact fn the headers handler calls) with a
  real Peer session: heavier branch adopted, judged claim moved onto the
  Peer, branch source label recorded.
- the two existing trigger tests updated to the new 6-arg signature with
  `None` verdicts (pre-change semantics pinned).

## Verification

- `cargo test --lib` — full suite green: **722 passed, 0 failed, 1 ignored**
  (complete output bundled as cargo-test-output.txt; 717 pre-fix → +5 new
  tests across r1-r6). `cargo build --release --bin follow_tip` also
  builds clean (the deploy binary).
- Deploy + live-verify plan: fleet-wide binary swap (DNS2/Pi/DNS3 Linux units,
  sami-pc CI artifact for follow_tip.exe); live proof = `stats.inbound_polls`
  advancing on DNS2 (which holds the PC as an inbound i2p/onion peer),
  `getpeerinfo.claimed_tip_height` populated for the PC row, four-way parity
  EXACT at the shared height, PC balance standing number 5110.900381.

## R6 ERRATA (answering the round-5 Codex blockers verbatim)

Round-5 grade: C, urn:ump:4f5vz4xksppxtfz5hv3uuniazwlzendvwvagsclvlgqag2y7pcwq
(the round-5 judge verified the full tree applies exactly, the transcript's
722 outcomes with all named item-21 regressions, and the core implementation).

1. **"DropPeer clears branch_source unconditionally — an unrelated peer's
   failed reconciliation erases the advertiser preference"** — AGREED and
   FIXED: the DropPeer arm now clears ONLY when the evicted peer IS the
   recorded source (`branch_source.as_deref() == Some(peer.label.as_str())`).
   Regression: `an_unrelated_peers_droppeer_never_erases_the_adoption_source`
   — A adopts; B's recon escalates to DropPeer (unknown-prev batches x
   RECON_MAX_UNRESOLVED+1); the source STILL names A and A remains the
   resolved getdata target; then a peer carrying the SOURCE label reaches
   DropPeer and only then the preference clears (the single erasure path).
2. Polish taken, all three:
   - the poll loop now `continue`s past a failed send (one wedged peer no
     longer starves the rest of that tick's polls);
   - skill lesson + Peer doc say "every NON-EMPTY headers reply" (an empty
     reply intentionally retains the preceding claim — which is what the
     verdict measures);
   - the adoption-source clear now also requires the in-flight `outstanding`
     set to be EMPTY — a getdata sent but not yet validated keeps its
     advertiser preference for a re-request (retention until validation,
     as suggested, not merely until the queue moved into outstanding).

## R5 ERRATA (answering the round-4 Codex blockers verbatim)

Round-4 grade: D, urn:ump:2comrofgoafvuy6zqyszubbbltyl5cefs6bqdiurq2pvdghwdtla
(round 3 was an INFRA fail — Codex returned malformed JSON,
urn:ump:g54i7hgy3m54…; a trimmed re-submission inherited the bundle critique).

1. **"actual Cargo project layout … documented commands run from the
   artifact root"** — this bundle carries `tri7-judge-tree/` = a FULL clean
   project (git archive of master 2c84e34 + diff.patch applied: Cargo.toml,
   Cargo.lock, build.rs, src/ with every module INCLUDING
   src/bin/follow_tip.rs, vendor/, tests/, examples/). The round-5 judge
   confirmed `git apply --check --reverse` succeeds against it and the
   transcript's named regressions all pass. A fresh `cargo check --lib`
   on the staged tree compiles clean (59.2s).
2. **"RUNBOOK test count/timing must agree with the bundled output"** —
   FIXED: RUNBOOK now states the same numbers as the transcript (722
   passed / 69.29s, complete `> file 2>&1` capture, rc=0).
3. **"release-build claim"** — the claim is scoped honestly: the release
   build ran on the host tree (7.1 MB binary); the bundled tree's own
   verifiable proof is its clean `cargo check --lib`.
4. **"brief said whole src dir bundled when it was trimmed"** — the bundle
   description now states the exact contents per round; this round's
   description (tri7-judge-tree/ full project) is literal.

## R3 ERRATA (answering the round-2 Codex blockers verbatim)

Round-2 grade: C, urn:ump:u4trww2m7miymx4jsyq6izt7uxn44qlu4yfhwrbq2m2f7m75zjrq.

1. **"Test output incomplete — ends at 'Running unittests', no summary"** —
   FIXED: the round-2 bundle accidentally captured only cargo's stderr
   (`2> file`). This round's `cargo-test-output.txt` is a full
   `> file 2>&1` capture of `cargo test --lib` on the exact tree: rc=0,
   all 721 test lines, final `test result: ok. 721 passed; 0 failed;
   1 ignored`. (An earlier full-suite run hit ONE failure in the
   UNRELATED pre-existing test `gui::node_supervisor::tests::
   tor_child_spawn_reuse_and_kill_on_drop` — a parallel-execution race in
   gui code this diff does not touch; it passes solo 3/3 on the stashed
   pre-change tree and the re-run full suite is green. Noted here for
   honesty, not as part of this artifact.)
2. **"branch_source is global mutable state, overwritten by any later
   recon_dispatch"** — AGREED and FIXED. The source is now bound at the
   ONE adoption event only: `recon_dispatch` sets it in the Switched arm
   (the judge just queued the adopting branch's headers); the Ignored arm
   explicitly does NOT touch it (comment pins the r3 rule), Extended does
   not either, DropPeer clears it, and the main loop clears it the moment
   `pending_from_headers` drains (adoption satisfied). Regression:
   `an_intervening_recon_from_another_peer_never_steals_the_adoption_source`
   — A adopts (4 pendings queued, source=A) → an intervening lighter
   judgment from B → source STILL names A and the resolved body target is
   still A. (Test design note: B's lighter branch must fork BELOW the tip
   — above the rolled-back tip our above-fork work is 0 and anything wins;
   the test forks B at h101 for a tie → KeepOurs → Ignored.)
3. **"control-flow-level regression observing the actual getdata send
   target"** — ADDED: `resolve_body_send_target` now contains the EXACT
   selection the main loop performs (the loop calls it directly), and
   `adopted_body_getdata_arrives_on_the_advertisers_socket_not_the_outbounds`
   resolves against two REAL peer sessions, sends the getdata through the
   chosen session, and OBSERVES the frame bytes arriving on the inbound
   advertiser's accepted socket while the outbound peer's socket reads
   empty (frame bytes compared against `encode_message("getdata", …)`).
4. **"worktree lacks the Cargo project"** — r3 bundled Cargo.toml +
   Cargo.lock + the whole src/ dir (the r3 run itself died on a Codex API
   parse failure, urn:ump:g54i7hgy3m54…, before judging that); r4 trimmed
   to the single touched file and was correctly marked not-runnable. THIS
   round bundles the FULL project tree (`tri7-judge-tree/`: git archive of
   master 2c84e34 + diff.patch applied — Cargo.toml, Cargo.lock, build.rs,
   src/ with every module and the src/bin/follow_tip.rs target, vendor/,
   tests/, examples/) as a directory, so `cd tri7-judge-tree && cargo
   test --lib` reproduces the transcript from the artifact root; the
   staged tree's own fresh `cargo check --lib` passes (59.2s).
5. Polish taken: the stale "FIRST outbound peer" dispatch comment replaced
   with the r2/r3 semantics; the Peer.claimed_tip doc now says refreshed on
   every NON-EMPTY reply (empty replies retain the previous claim — which
   is what the verdict measures). Ordinary non-recon pendings inherit the
   adoption preference only while that adoption's pendings are still in
   the queue (they are the same branch's bodies), and the preference is
   cleared the moment the queue drains — documented in the dispatch
   comment and the ReconAcc field doc.

## R2 ERRATA (answering the round-1 Codex blockers)

Round-1 grade: C, urn:ump:qh6war7rnd5ccaowaphykkqd4d4gosrqy24byfoxk2kno5f42peq.

1. **"Route adopted-branch body requests to the peer that advertised or
   served those headers … outbound-first selection does not solve the
   documented case"** — AGREED and FIXED. `ReconAcc.branch_source` now
   records the label of the peer whose branch is being accumulated/judged
   (set in `recon_dispatch` for every recon material intake), and the
   pending-body dispatch calls the new `choose_body_source_label`:
   recon advertiser FIRST (when still live), then outbound, then any live
   peer. Outbound-first is now only the FALLBACK, never the primary when
   the advertiser is connected. Pinned by
   `body_source_choice_prefers_the_recon_advertiser_over_healthy_outbound`
   (the exact scenario: healthy outbound + inbound advertiser → inbound
   chosen).
2. **"Add an integration-level regression that proves an inbound peer is
   polled, its claim triggers reconciliation, and getdata is sent to that
   inbound peer"** — ADDED at the two layers that are unit-testable in this
   suite: (a) `recon_dispatch_transfers_claim_and_source_to_the_peer` runs
   the REAL dispatch (the headers-handler entry) against a real Peer
   session and proves claim transfer + source recording + adoption;
   (b) `choose_body_source_label` proves the getdata TARGET decision routes
   to the inbound advertiser. The poll itself (loop line → tip locator to
   every peer) and the socket send are unchanged pre-existing mechanics
   (the loop already sent to a filtered subset of the same `peers` vec);
   the decision logic that changed is fully covered. A full main-loop
   socket harness does not exist in this suite (the loop body is a ~1700
   line fn taking live RPC/store handles); we state that limit honestly
   rather than claim a test we do not have.
3. **"inbound_polls … attempts vs sent" polish** — counter now increments
   AFTER a successful send (same semantics as `stats.polls`; failed sends
   drop the peer and are logged, not counted). Comment updated.
4. **"source tree / test output absent; git apply --check fails"** — this
   round ships the full `src/node_embedded.rs` (the only touched file) plus
   the real `cargo test` output file, so the diff applies and the claims
   are checkable in-place.

## Risk notes

- Polling inbound peers adds one small getheaders per peer per 30 s tick (≤32
  peers) and their headers replies — no new protocol message types.
- The claim re-fires the re-ask at most once per 60 s per diverged peer (the
  same cooldown that governs the item-19 streak), and an in-flight re-ask's
  empty echo never counts as evidence (the r5 blocker guard is untouched).
- `stats.polls` semantics unchanged (outbound polls only); `inbound_polls` is
  additive and success-only.
- A stale `branch_source` label (after the branch converged) only biases the
  body-fetch choice toward a live peer that holds canonical anyway; a gone
  peer falls through the ranks by label miss.

