# Judge brief: tri7 lane-drawer+sync — drawer overlay + attach-mode sync indicator design fix

## Two operator-reported issues, one branch (base abb13e1 = current master)
1. "The tor-i2p drawer shouldn't show receive coins" (screenshot): the Receive
   Coins page hosted the Tor/I2P addresses block, and the status-bar chip
   FORCED navigation to Receive Coins to show it.
2. "pc not synced yet?" (screenshot): the sync bar showed "2208662 blocks
   (daemon 2208687)" red — a LOCAL tipstate snapshot (stale, taken at GUI
   start) compared against the daemon's live tip. The operator rejected the
   design: in attach mode there is no local chain; the GUI must report the
   DAEMON's live state only.

## The design fix (verified: no standalone path exists)
The GUI never runs an embedded node (node_supervisor spawns an external tri7d;
all chain knowledge arrives via RPC). st.tip was only a stale tipstate.json
file snapshot. Per the directive, the local-vs-daemon comparison code was
DELETED entirely (tipstate loading itself STAYS — it feeds coin
depth/maturity in balance_split, which already prefers the daemon height).

## What changed (+335/-125 across the three GUI files — the current combined diff vs base abb13e1, includes the round-2 state fix + probe plan extraction)
1. **Drawer state into AppState** (headless-testable): net_drawer_open +
   net_chip_clicked() (pure toggle — the chip handler contains NO Screen
   assignment anymore), net_drawer_touch() (Copy refresh), net_drawer_tick()
   (THE one auto-close timer).
2. **net_drawer_overlay(ctx)**: the drawer body moved verbatim (ONE copy,
   grep-asserted) into an egui::Window anchored RIGHT_BOTTOM above the 40px
   status bar, called unconditionally in update() — renders on every screen.
   Receive Coins no longer references it.
3. **daemon_sync_display()**: attach-mode sync display = daemon live height +
   daemon syncing flag ONLY. syncing:true → live walk (Syncing H/T · rate ·
   ETA), progress < 1.0; synced → 1.0 / "Synced · daemon <live height>"; no
   daemon → 0.0 / "no tri7d RPC". The local snapshot is never a fallback.
   Also fixed a latent double-prefix ("Synced · Synced · daemon N").
4. Status bar: green (SUCCESS) only when the daemon reports synced; amber/red
   only while syncing or unreachable. The Spendable-area warning was the same
   sync-gap cluster — now keyed off the daemon's syncing flag only.

## Tests (+3: 2 drawer + 1 sync)
- net_chip_toggles_the_drawer_without_navigating (behavioral: toggle from
  Transactions AND Receive — st.screen never moves; tick closes stale,
  fresh touch survives).
- receive_page_no_longer_renders_the_tor_i2p_drawer_block (source-level seam:
  page_receive no net_drawer refs; statusbar no Screen::Receive; drawer body
  marker occurs exactly once).
- attach_mode_sync_display_is_daemon_only: stale local tip 2208662 vs daemon
  2208687 → daemon height renders while syncing; syncing:false renders
  green/1.0 (the OLD code rendered a red gap in exactly that state);
  "2208662" / "blocks (daemon" appear NOWHERE in the attach path, including
  unreachable states.

## Parent verification (adopting session, private target dir)
FULL cargo test --lib: **553 passed; 0 failed; 1 ignored** (drawer round-1
baseline 552 + 1 sync test). Scope: the three GUI files only (+312/-129).
"blocks (daemon" grep on the merged app.rs: 0 occurrences (the old format is
gone); 1 doc-comment mention of the deleted code remains in app_state.rs.

## Files in this bundle
brief.md; drawer-sync.diff (current combined diff vs base abb13e1);
tests-drawer-sync-r3.txt (COMPLETE unfiltered cargo test --lib output with
the exact command, worktree/branch/base identity, and an explicit statement
that the state under test is base + uncommitted modifications — the parent
commits it post-grade as one commit).

## ROUND 2 — judge blocker addressed
daemon_sync_display now handles Some(GuiSync { syncing: false, .. }) EXPLICITLY
as the healthy FOLLOWING state: green/1.0 with the daemon's live height
(the round-1 code fell into the syncing arm for that shape and stayed red
forever). The regression test now constructs that EXACT state (before it
substituted sync=None and dodged the defect), plus the None fallback stays
covered. Evidence: tests-drawer-sync-r2.txt (full-suite tail, 553 passed).
Suite command: CARGO_TARGET_DIR=/root/tri7-lanes/target-drawer cargo test --lib
in worktree /root/tri7-lanes/lane-drawer @ branch lane/drawer (base abb13e1).

## ROUND 3 — evidence depth
tests-drawer-sync-r3.txt now carries: exact worktree path + branch + base
commit, the git status of the changed files, the exact command, and the full
filtered cargo test --lib output (not a summary line).

## ROUND 5 — judge blockers addressed
1. The GUI-FEATURE build ran and its output is bundled: `cargo check --bin
   tri7_wallet_gui --features gui` — Finished clean (src/gui/app.rs including
   net_drawer_overlay COMPILED, not just searched).
2. Captured git evidence in tests-drawer-sync-r3.txt: git rev-parse HEAD,
   branch, git status --short, and diff --stat — verbatim.
