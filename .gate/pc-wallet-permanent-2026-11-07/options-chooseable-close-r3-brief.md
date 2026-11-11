# Options are chooseable again + close minimizes + Explorer compass (round 3)

Operator reports, in order:
(a) screenshot of Options -> Display showing a red "Options not applied"
    banner reading: method settxfee is not on the GUI read-only allow list.
    He asked: I want my wallet to minimize on close, and make sure the options
    are chooseable.
(b) "Can you put a compass symbol next to the block explorer. It doesn't have
    a symbol next to it"

(a) ROOT CAUSE (two defects, one visible symptom):
1. The GUI pushes the typed fee to the node with RPC settxfee
   (app_state.rs push_paytxfee_to_node). The daemon IMPLEMENTS settxfee
   (follow_tip.rs, range-validated 0..0.25 TRI, moves no coins), but the
   GUI's client-side allow list (src/gui/rpc_client.rs ALLOWED_METHODS) did
   not contain it - so the call was refused BEFORE a byte hit the wire and
   save_options_action returned Err.
2. save_options_action treated that fee push as a GATE on the whole apply:
   it returned Err, so the Options dialog refused to complete ("Options not
   applied") and the operator could not change ANY option - although the
   options were in fact written to disk (options.json showed
   minimize_on_close/tray_on_close already true).

(a) FIXES:
- rpc_client.rs: settxfee added to ALLOWED_METHODS (the daemon validates the
  range; it moves no coins). The fee now actually reaches the node.
- app_state.rs save_options_action: PERSIST FIRST, then push the fee as a
  best-effort node sync. A node-side fee failure is now a NOTICE
  (options_fee_notice) - the options are applied and the dialog completes.
- app_state.rs: new options_fee_notice field; cleared BEFORE each fee push
  (Codex r1 blocker 1) so a stale warning cannot survive a successful retry.
- app_state.rs: the fee is now pushed to the node once the node first becomes
  reachable in a session (push_saved_fee_when_node_appears), so a fee saved
  while the node was down is not local-only forever (Codex r2 polish).
- app.rs: the notice renders as a WARNING card ("Options saved - the node has
  not taken the fee yet"), not the "Options not applied" error.
- app.rs close handler: now driven by AppState::close_decision() - minimize_on
  close OR tray_on_close BOTH minimize and keep the process + node alive
  (previously a stale options.json could let the window die and take the
  supervised node with it). With both flags off the window closes for real.
- app.rs Display panel: the radios are relabelled honestly ("Minimize to the
  taskbar on close (keep running)" / "Quit Triangles (stop the node)").

(b) ROOT CAUSE: Screen::Explorer mapped to the menu_16 icon stem "search",
but "menu_16/search" was NEVER in the GUI asset map (src/gui/app.rs
asset_map!) - so the texture silently failed to load and the Block Explorer
row rendered with NO symbol at all. Fix: a new compass asset
src/gui/assets/menu_16/explorer.png (16x16 RGBA, drawn in the legacy brand
orange #F26522 like the neighbouring menu icons) plus
  - asset_map!: "menu_16/explorer" => "menu_16/explorer.png"
  - Screen::Explorer => "explorer" (state.rs)
The rendered compass (ring + NE/SW needle) was visually verified at 12x.

ROUND 3 fixes the two Codex round-2 blockers:
- BLOCKER 1 (the stale-warning test never exercised a successful push): the
  test now runs a REAL loopback stand-in for the node that ACCEPTS settxfee
  (a TcpListener answering the tri7d envelope), so the second apply performs
  a genuinely successful push and the test asserts the stale notice is GONE.
  The manual `options_fee_notice = None` line is gone.
- BLOCKER 2 (the brief claimed a successful retry was tested): this brief
  now describes exactly what the test does.
- POLISH (close test only restated the boolean): the decision now lives in
  AppState::close_decision() -> CloseDecision (the REAL path the X button
  uses) and the test drives that function for all four flag combinations.
- POLISH (the "picked up on the next launch" claim): now TRUE by
  construction - push_saved_fee_when_node_appears() pushes the saved fee the
  moment the node answers, and a test proves it.

TESTS (6 in src/gui/tests.rs, all new):
- options_apply_persists_even_when_the_node_fee_push_fails
- options_fee_notice_is_cleared_by_a_later_successful_apply (real mock node)
- saved_fee_is_pushed_when_the_node_appears (real mock node)
- close_behaviour_flags_cover_every_combination (drives close_decision())
- settxfee_is_on_the_gui_allow_list
- plus the existing GUI suite unchanged.

cargo test --release --lib: 496 passed, 0 failed (491 at the start of this
work, +5 new).
