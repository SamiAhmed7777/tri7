# Options are chooseable again + close minimizes (operator-reported)

Operator's screenshot of Options -> Display showed a red "Options not applied"
banner reading: method settxfee is not on the GUI read-only allow list.
He asked: I want my wallet to minimize on close, and make sure the options are
chooseable.

Root cause (two defects, one visible symptom):
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

Fixes:
- rpc_client.rs: settxfee added to ALLOWED_METHODS (the daemon validates the
  range; it moves no coins). The fee now actually reaches the node.
- app_state.rs save_options_action: PERSIST FIRST, then push the fee as a
  best-effort node sync. A node-side fee failure is now a NOTICE
  (options_fee_notice) - the options are applied, the dialog completes, and
  the fee is picked up on the next launch.
- app_state.rs: new options_fee_notice field (cleared on a successful apply).
- app.rs: the notice renders as a WARNING card ("Options saved - the node has
  not taken the fee yet"), not the "Options not applied" error.
- app.rs close handler: minimize_on_close OR tray_on_close now BOTH minimize
  and keep the process + node alive (previously a stale options.json could let
  the window die and take the supervised node with it). With both flags off
  the window closes for real.
- app.rs Display panel: the radios are relabelled honestly
  ("Minimize to the taskbar on close (keep running)" / "Quit Triangles (stop
  the node)"), and the tray option states that it behaves like minimizing
  until the tray helper lands.

cargo test --release --lib: 491 passed, 0 failed. Evidence: the diff, the
operator's screenshot (red banner + allow-list error), the live options.json
(both flags already true), and the daemon's settxfee implementation.
