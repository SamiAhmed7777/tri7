# tri7 lane: six legacy RPC ops-wiring verbs (addnode, disconnectnode, getseedlist, getnetworkstability, gettxout, getrawchangeaddress)

Legacy parity (v6.2.6.7 rpcnet.cpp/rpcblockchain.cpp, cloned read-only at /tmp/rpcops-ref for exact shapes). All NATIVE_METHODS/OPS_METHODS edits are pure appends (parallel-lane safe). Byte-exact legacy error strings where the legacy defines them ("Node already added.", "Node not found in addnode list.", "Only .onion or .b32.i2p…", "Node not found."). Disclosed deviations, never invented data: disconnectnode's table syncs every 2s (seconds-old sessions may be invisible); getnetworkstability ping -1.0/peers_measured 0 (tri7 measures no RTT) + additive peer_quality; gettxout + getrawchangeaddress are ABSENT from the v5 table (Core-0.8 shapes, `tri7_source` disclosure); gettxout resolution mempool → wallet live view → authoritative addrindex (fail-closed complete+at-tip) → disk store, unknown tx → null, bad vout → -8; getrawchangeaddress follows the getnewaddress ladder, labelled "change".

FILES: src/rpc.rs (+49: NATIVE 202-215, OPS 303-313, routing test 5598-5621), src/node_embedded.rs (+935: state-dir peer-store helpers, OpsAdapter fields, six dispatch arms, PeerCtl loop drain, connected_at, verb helpers, tests), src/peers.rs (+36: remove/contains + test), src/addrindex.rs (+50: OutpointRow, authority_at, outpoint).

TESTS (pass by name): rpc_ops_wiring_verbs_are_native_and_ops_routed, addnode_add_remove_on_the_store, addnode_parse_endpoint_accepts_overlay_and_rejects_the_rest, isolation_risk_bands_match_legacy, endpoint_matches_labels_and_table_endpoints, getseedlist_rows_carry_the_legacy_shape, txout_coin_json_shape.

PARENT-VERIFIED (fresh run in the lane tree, own target dir): `cargo test --lib` → `test result: ok. 662 passed; 0 failed; 1 ignored` (655 base + 7 new).

TREE: branch lane/rpc-ops-wiring, worktree /root/tri7-lanes/lane-rpcops, base be3cd3c; diff scoped to the four files (lane.diff). Repo lane only — no deployment.

## Round 2 (judge blockers resolved, urn:ump:3onfo6itwkv3n3lpqjbawqapf6kqt2zb77xma3z7q4kcsen2ofja)
1. getrawchangeaddress now validates argument count — any argument returns (-8, Core-style usage help disclosing the verb is not in the v5 table).
2. Peer-store SAVE FAILURES propagate: new strict persist_peer_store_at; addnode add/remove surface `addnode: peer store save failed: {e}` instead of acknowledging unpersisted updates (legacy dup/not-found errors keep precedence).
3. Load-modify-save serialization: process-wide PEER_STORE_IO OnceLock<Mutex<()>> gate held across BOTH with_peer_store_at and persist_peer_store_at — RPC threads and the peer loop can no longer interleave and resurrect stale snapshots. RED-proof: with the gate lines removed the new concurrent test FAILS (lost updates); restored, it passes.
+10 tests (671 passed / 0 failed / 1 ignored — full raw run in full-suite-r2.log), incl. six verbs now routed through real NodeOps::call via a test-only ops_adapter_for_tests harness (replacing the mirror-style getseedlist test — polish taken), corrupt-peer-store recovery, and concurrent-writes-no-lost-updates.
