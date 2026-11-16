# Fleet-resilience code fixes — 2026-11-13 live session

Base commit: 0d26ac2 (HEAD before edits). This diff touches TWO files, both
network/supervision/boot-layer. NO consensus file is touched: no block
validation, no fork-choice arithmetic, no kernel/staking rules, no wire
format. The chain's consensus behavior is byte-identical; these fixes change
WHOM the node dials and WHEN, and how the wallet supervises its bundled
tor/i2pd proxies.

## Live fleet evidence this session (captured 2026-11-13 ~11:00-11:25 UTC)

- LAPTOP (sami-pc sibling tri7_wallet_gui, C:\Coins\TRI-LAPTOP): node froze
  beside a stale peer entry — log repeated tip-poll + recon lines with
  `out:...b32.i2p:24113 claims h2210964 (we are h2210988)` while the node
  never re-dialled; the interim stallwatch task force-restarted the GUI 13+
  times overnight (00:26 → 03:05 local), each taskkill /F tearing the store
  (body hole at h2210955: `wallet live seed retrying: store missing block
  body at h2210955` every 5s). The stallwatch script header itself names the
  defect: "the embedded node reconnect defect (node never re-dials after its
  last peer dies)". This is OPEN DEV ITEM 1 in TRI7-COORDINATION.md.
- SAMI-PC: TWO i2pd processes running simultaneously (SYSTEM task
  C:\Users\hello\tri7\i2pd\pkg\i2pd.exe since 11/11 + GUI-bundled
  E:\Coins\TRI\bin\i2pd\i2pd.exe spawned 2:07 AM) — the surviving trusted one
  had NO SOCKS listener on 4447 (`netstat -ano | findstr :4447` shows only
  TIME_WAIT client sockets) while the process stayed alive, so every .b32.i2p
  dial failed and the node dropped its i2p peer at 11:02:45Z ("stale link").
- LAPTOP tor: orphan tor.exe (PID 18196, since 11/11) healthy on the OLD
  socks port while the supervisor had regenerated torrc with a NEW random
  SocksPort and the spawn of a second instance died on the first one's
  DataDirectory lock — chip reported "Tor unavailable" for days while onion
  dials through the orphan actually succeeded (peers.json ok_count 25).
- DNS2 (this host): tip frozen at h2210963 for 20+ min with the journal
  spamming `adoption body dispatch: no live peer can take N body request(s)
  — requeued (no pin)` every second while connected peers claimed taller
  tips; a daemon restart unstuck it — the restart's only useful move is a
  fresh handshake, which no in-band path performed.

## The four fixes

### FIX 1 — dial-rotation gate counts only HEALTHY outbound peers (src/node_embedded.rs)

The poll-tick gate was `peers.iter().any(|p| p.outbound)`: a dead-but-not-yet-evicted
entry (sess_ok() = !dead can stay true through a hung session) or a stale entry kept
`have_outbound` true and suppressed the rotation forever.

- NEW pure fn `fresh_outbound_present(peers, now, fresh)` — an outbound peer
  suppresses the rotation only while not dead AND traffic is fresh within
  `OUTBOUND_FRESH_WINDOW` (240s; the keepalive pass runs every POLL_SECS and
  the stale-link prune cuts at STALE_LINK_SECS, so a healthy peer is always
  well inside). Clock-skew-safe (checked_duration_since; future traffic counts
  fresh). Wired into the poll tick in place of the bare `any(p.outbound)`.
- New consts: `OUTBOUND_FRESH_WINDOW`.

### FIX 2 — adoption no-server escalation dials a fresh seed (src/node_embedded.rs)

The body-dispatch `None` arm ("no live peer can take N body request(s)")
requeued forever — the connected-but-non-serving peer set never refreshes.

- The None arm now marks `no_server_this_pass`; after the pass, a pure
  decision fn `adoption_escalation_due(no_server_since, last_dial, now, grace,
  spacing)` fires a rate-limited `try_connect` of a fresh transport seed
  (grace 60s: demotion-window expiry `expire_body_demotions` gets first
  chance; spacing 60s: no dial storm — the dispatch pass can run multiple
  times per second). New consts: `BODY_NO_SERVER_GRACE`,
  `BODY_ESCALATION_DIAL_SECS`. Loud log: "adoption body ESCALATION: no
  connected peer could serve the adopted branch — dialling a fresh transport
  seed". The escalation clock resets when a pass finds a server AND when the
  adoption completes (pending+outstanding empty) so starvation never carries
  across adoptions.

### FIX 3 — proxy supervision: pinned port, data-trust child health, orphan adoption (src/gui/node_supervisor.rs)

- TorSupervisor: the bundled SOCKS port is PINNED
  (`pub const BUNDLED_TOR_SOCKS_PORT: u16 = 58033` — the port the deployed
  laptop confs already carry) instead of `free_port()` per session. The conf
  socks= line (written by ensure_tor_conf_lines from the ensure result), the
  generated torrc and any surviving child/orphan now agree forever.
- ensure() ordering (i2p lane's r2 child-first rule applied to tor):
  1. tracked child kept ONLY while its SOCKS port actually answers
     (data-trust, never process-trust); alive-but-dead → killed + respawned
     below (the sami-pc i2pd case);
  2. system tor on 9050 adopted (unchanged);
  3. NEW: a surviving orphan on the pinned port ADOPTED, never spawned over;
  4. latch/attach gate (unchanged); spawn onto the pinned port with the
     generated torrc; ONE Windows stale-image retry (taskkill /IM tor.exe)
     when the first spawn fails — the DataDirectory-lock recovery for a
     broken orphan (step 2 already adopted any healthy system tor).
- The spawn+wait path is extracted into `spawn_and_wait()` so the retry
  re-runs the identical path.
- I2pSupervisor: same data-trust upgrade — its child-first step trusted
  `try_wait()` (process liveness) with NO port probe; now it probes the
  pinned 4447 and kills+respawns a live child whose socksproxy died.
- Tests: tor_supervisor() helper now also points bundled_probe_port at a
  private free port so parallel tests never collide on 58033; free_port()
  (now test-only) is #[allow(dead_code)] with an updated doc.

### FIX 4 — genesis-only store is a bare store (VERIFIED ALREADY LANDED, no code in this diff)

The owed `store_usable` fix (genesis-frame-only store must read as bare so
from-zero resumes — laptop h0 wedge) is ALREADY in the tree at base
0d26ac2: `store_has_bodies(blocks_file)` (src/node_embedded.rs:10772) wired
at the boot gate (src/node_embedded.rs:~10898) with the hollow-state comment,
plus the supervisor-side twin `is_fresh_store` in src/gui/node_supervisor.rs
(reads DATA, never file existence). Verified by reading both sites at the
base revision; no change needed — listed here for completeness of the
session's fix inventory.

## Round-2 judge verdict (F, urn:ump:kjgm333e6az77k4aedzm2c545xjandp2gqu5is4ziwtzvtfkaw3q) and resolutions

- BLOCKER "image-wide `taskkill /IM tor.exe`" — ACCEPTED. Fixed: recovery now
  inventories tor processes via PowerShell (`Get-Process tor` → id|path rows)
  and kills ONLY processes whose exe path equals our bundled tor.exe
  (case-insensitive), via `taskkill /PID <pid>`. The pure decision fn
  `wallet_owned_tor_pids` is unit-tested on every host
  (`image_kill_targets_only_wallet_owned_tor_processes`: ours → killed, Tor
  Browser / user daemon / legacy other install / unknown-path rows → spared).
  With no matching process nothing is killed and the supervisor latches out
  with the loud remedy line.
- BLOCKER "socks_endpoint_answers only accepts TCP, no SOCKS handshake" —
  REBUTTED with source: the function (node_supervisor.rs, the "REAL SOCKS5
  greeting" block) sends the 05 01 00 no-auth method request and REQUIRES the
  05 00 choice reply — a bounded SOCKS protocol handshake, not a bare TCP
  accept. The worktree copy is provided; verify at `socks_endpoint_answers`.
- BLOCKER "adopt the pinned endpoint without identity" — ACCEPTED (hardened):
  adoption now requires our GENERATED torrc on disk (`state/torrc`) as the
  ownership witness in addition to the pinned port + SOCKS5 handshake; an
  unidentified port resident is never adopted. New test
  `tor_does_not_adopt_a_pinned_port_resident_without_ownership_witness`.
- BLOCKER "escalation doc vs any-group-starved code" — ACCEPTED: doc aligned
  to the implementation (a pass where ANY group finds no live server is a
  starving pass; grace window + dial spacing bound the rate so the wider
  trigger cannot storm), on `adoption_escalation_due` and the call site.
- POLISH accepted: stale i2p-supervisor comment ("Tor takes a fresh free
  port per spawn") retired — tor is pinned now.
- The Windows-only recovery path cannot execute on Linux CI; its DECISION is
  extracted pure and tested (`wallet_owned_tor_pids`), and the process
  inventory (`tor_process_inventory`) is cfg-split with an empty non-Windows
  stub.

## Round-3 judge verdict (F, urn:ump:unayqfhgsnvdxfhmsa5zc4ua34m5ot7qj5ixiutykmrmuo4tnfoa) and resolutions

- BLOCKER "torrc-existence adoption is unsound — a stale file does not
  identify the port's serving process" — ACCEPTED. Adoption is now
  PROCESS-BOUND: the pinned port's LISTENER pid must resolve to an executable
  path equal to our bundled tor.exe (`listener_of_socks_port_os`:
  Get-NetTCPConnection OwningProcess + Get-Process Path on Windows;
  case-insensitive match in the pure `listener_is_wallet_tor`). The torrc on
  disk is demoted to a SECOND witness, never sufficient alone. Adversarial
  test: `orphan_adoption_requires_process_bound_listener_identity` (Tor
  Browser / user daemon / legacy other install / unresolvable / empty-path
  listeners are ALL rejected).
- BLOCKER "spawn_and_wait attributes any handshake on the pinned port to the
  spawned child (foreign-listener race)" — ACCEPTED. `spawn_and_wait` now
  attributes the handshake: where the listener is resolvable it must be the
  spawned CHILD's pid (`listener_pid_is`); a foreign answer keeps the loop
  waiting and, on failure, names itself in the reason ("SOCKS … answered by a
  NON-child process (foreign port resident) — refusing to claim it as our
  tor"). Where the listener is unresolvable (non-Windows hosts — which ship NO
  bundled tor payload at all), the real handshake remains the gate; this is
  documented in-code at both sites.
- BLOCKER "adversarial tests for stale-witness + foreign listener, and a
  briefly-alive failing child" — ACCEPTED: the no-witness test now uses an
  exiting stub (real tor's bind-failure behavior) precisely so the wait
  CANNOT latch onto a foreign listener; pure identity tests cover
  foreign-path / unresolvable / empty-path cases; the a3 adoption test drives
  the process-bound path through an injected resolver.
- POLISH accepted + fixed: the stale `socks_endpoint_answers` doc ("a bare
  accept still counts") — that doc was WRONG and is exactly what misled the
  round-2 review; the implementation has required the `05 01 00` → `05 00`
  handshake all along. Rewritten to match the code. Attach-mode doc clarified
  (may adopt a process-bound orphan; never spawns). Stale i2p "free port per
  spawn" comment retired (r2 polish, kept).
- Round-2 blocker resolutions stand: ownership-safe recovery now kills only
  the LISTENER of the pinned port when its exe path is the bundled tor.exe
  (`taskkill /PID`); escalation doc aligned (r2).

## Round-4 judge verdict (C, urn:ump:rqtlrt42a56qqb3p345xri37kdeshqr6e2r2552tguzpde6cicnq) and resolutions

- BLOCKER "attribution fails OPEN on Windows when listener resolution returns
  None" — ACCEPTED. Both the tracked-child health probe and `spawn_and_wait`
  now FAIL CLOSED on Windows: `None => false` under `#[cfg(windows)]`; the
  handshake-only fallback survives ONLY under `#[cfg(not(windows))]` (hosts
  that ship no bundled tor payload at all) and is a compile-time branch, not
  a runtime maybe. A transient PowerShell failure can no longer hand the
  wallet to a foreign proxy.
- BLOCKER "pid and exe path from two separate queries can pair one process's
  pid with another's path" — ACCEPTED. `listener_of_socks_port_os` now
  captures the connection and the process identity in ONE PowerShell query
  (single `Get-NetTCPConnection … | Select -First 1` + `Get-Process -Id
  $c.OwningProcess`, one pid|path line).
- POLISH accepted: BRIEF verification numbers updated to the final 771-run
  with the failed intermediate runs distinguished; the TorSupervisor struct
  doc no longer describes "port + torrc prove identity" — it now states the
  process-bound identity + fail-closed semantics.

## Consensus preservation

`git diff --stat` for this change: src/gui/node_supervisor.rs and
src/node_embedded.rs ONLY. Neither diff hunk touches: block validation,
hash9, kernel/stake rules, difficulty, fork-choice work comparison, wire
encoding, checkpoint constants, or the RPC surface. The fixes alter dial
targets/timing and proxy supervision only.

## FILES

- src/node_embedded.rs
- src/gui/node_supervisor.rs

## Verification (this working tree, base 0d26ac2)

1. `cargo check --lib` — Finished dev profile, 0 errors (55 pre-existing
   warnings, none introduced; log: gates.log).
2. `cargo test --lib` — **771 passed; 0 failed; 1 ignored (94.8s)** on the
   FINAL tree (gates.log tail, EXIT_TEST=0 — the authoritative record).
   Run history in that log, annotated in fix order: (1) a parallel-test port
   collision (the tor wiring test used the production default 58033, inside
   the Linux ephemeral range; fixed with a private bundled port); (2) test
   infidelity — an exit-less stub child let the bounded wait latch onto a
   FOREIGN port resident's answers; the stub now mirrors real tor's
   bind-failure behavior; (3) after the r3 process-bound identity work the
   no-witness test was upgraded to inject a FOREIGN listener identity so the
   attribution refusal itself is exercised (the exact foreign-listener race
   the round-3 verdict demanded covered). NEW tests all green:
   rotation_gate_counts_only_fresh_healthy_outbound_peers,
   adoption_escalation_fires_after_grace_and_spaces_dials,
   tor_child_alive_with_dead_socks_is_restarted_not_reported,
   tor_adopts_surviving_orphan_on_pinned_port_without_spawning,
   tor_does_not_adopt_a_pinned_port_resident_without_ownership_witness,
   torrc_carries_the_pinned_bundled_port_not_a_fresh_free_port,
   orphan_adoption_requires_process_bound_listener_identity,
   spawn_success_is_attributed_to_the_spawned_child_pid,
   i2p_child_alive_with_dead_socks_is_restarted_not_reported — plus the full
   prior suite (762 at base) which stays green.
3. `CARGO_TARGET_DIR=target-gui cargo check --bin tri7_wallet_gui --features
   gui` — EXIT_GUI=0 (gates.log) — GUI compile gate (node_supervisor.rs is
   gui-feature-gated).
4. `cargo build --release --target x86_64-pc-windows-gnu --bins` — EXIT_WIN=0
   (gates.log, Finished release) — the windows-gnu portability gate (all
   bins).
