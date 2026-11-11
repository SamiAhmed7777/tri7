# Options are chooseable again + close minimizes + Explorer compass (round 4)

Operator reports:
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
  The notice is cleared BEFORE each fee push, so a stale warning cannot
  survive a successful retry.
- app_state.rs: the fee is pushed to the node once the node first becomes
  reachable in a session (push_saved_fee_when_node_appears); a successful
  save_options_action push also sets fee_pushed_to_node, so the startup push
  does not repeat it.
- app.rs: the notice renders as a WARNING card ("Options saved - the node has
  not taken the fee yet"), not the "Options not applied" error.
- app.rs close handler: driven by AppState::close_decision() - minimize_on_close
  OR tray_on_close BOTH minimize and keep the process + node alive (a stale
  options.json could previously let the window die and take the supervised node
  with it). With both flags off the window closes for real.
- app.rs Display panel: the radios are relabelled honestly ("Minimize to the
  taskbar on close (keep running)" / "Quit Triangles (stop the node)").

(b) ROOT CAUSE: Screen::Explorer mapped to the menu_16 icon stem "search",
but "menu_16/search" was NEVER in the GUI asset map (src/gui/app.rs
asset_map!) - so the texture silently failed to load and the Block Explorer
row rendered with NO symbol at all. Fix:
- NEW asset src/gui/assets/menu_16/explorer.png (16x16 RGBA compass, drawn in
  the legacy brand orange #F26522 like the neighbouring menu icons; rendered
  and visually verified at 12x)
- app.rs asset_map!: "menu_16/explorer" => "menu_16/explorer.png"
- state.rs: Screen::Explorer => "explorer"  (this file IS part of the diff)

ROUND 4 fixes the three Codex round-3 findings:
- BLOCKER (the state.rs change was missing from the round-3 bundle): the diff
  now includes src/gui/state.rs, where Screen::Explorer maps to "explorer".
- POLISH (the brief said 6 tests, the diff has 5): corrected to 5 below.
- POLISH (the stand-in accepted any request): it now asserts the request body
  really contains "method":"settxee" - so the test proves the GUI calls
  settxfee, not some other verb.
- POLISH (once-per-session claim was not strictly true): a successful
  save_options_action push now also sets fee_pushed_to_node.

TESTS (5 new, in src/gui/tests.rs):
- options_apply_persists_even_when_the_node_fee_push_fails
- options_fee_notice_is_cleared_by_a_later_successful_apply (a real loopback
  stand-in node that ACCEPTS settxfee; the second apply performs a genuinely
  successful push and the stale notice must be GONE)
- saved_fee_is_pushed_when_the_node_appears (same stand-in)
- close_behaviour_flags_cover_every_combination (drives close_decision(),
  the real path the X button uses)
- settxfee_is_on_the_gui_allow_list

cargo test --release --lib: 496 passed, 0 failed (491 before this work, +5).
