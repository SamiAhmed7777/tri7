# tri7 lane: SSE event push — GET /events on the loopback RPC listener (pure Rust, no new deps)

Legacy-parity audit's last missing v6-extra class (zmqpublishnotifier.cpp hashblock/hashtx pub). ZMQ would need a C-linked dep (windows-gnu cross-build pain) — the pure-Rust answer is SSE on the existing hand-rolled HTTP server. DESIGN: `src/events.rs` (NEW, 195 lines) — EventHub = Mutex<HashMap<id, SyncSender>> + AtomicU64; publish() is TRY_SEND-only: a full/dead subscriber is dropped inside that publish; producers NEVER block (SUBSCRIBER_CAP 16, CHANNEL_BOUND 64, 15s keepalive). Hub via live_hub_or_init() OnceLock (same pattern as LIVE_STORE). GET /events branches in serve()'s connection closure after a head PEEK (bytes untouched for non-/events); no live engine → honest 404. Disconnect detection = write error OR sse_conn_alive() probe that DRAINS client bytes (parked GET head otherwise masks FIN forever — found by instrumentation, the real bug). Producers: 6 one-liners (block accept in advance_with_block; 4 pool.accept tx sites; staking toggle). /events rides the loopback-only bind (documented in the module doc).

TESTS (real code paths): sse_frame_shape_through_the_real_dispatcher (real serve() + loopback sockets: exact frame bytes, cleanup after disconnect), sse_without_a_live_engine_is_an_honest_404 (ops None AND stub), slow_consumer_is_dropped_and_producers_never_block (64-frame fill → 65th drops only the slow sub; healthy sub gets every frame), subscriber_cap_is_enforced_and_slots_free_on_unsubscribe, events:: frames_are_data_line_plus_blank_line, producer_helpers_noop_without_a_live_hub. Known limits (disclosed): sync-flood bursts during from-zero replay (try_send design, same as legacy zmq during IBD); staking event on RPC toggle only; hub once-per-process.

PARENT-VERIFIED (fresh run in the lane tree, own target dir): `cargo test --lib` → `test result: ok. 661 passed; 0 failed; 1 ignored` (655 base + 6 new).

TREE: branch lane/event-push, worktree /root/tri7-lanes/lane-events, base be3cd3c; diff scoped to src/events.rs src/lib.rs src/rpc.rs src/node_embedded.rs (lane.diff). Repo lane only.

## Rounds 2-3 (judge-driven fixes)
- R2 blocker RESOLVED: lane.diff now INCLUDES src/events.rs (was untracked; intent-to-add), diffstat regenerated — 4 files, 640 insertions.
- R2 polish taken: peek_sse_path requires an HTTP/1 version token before accepting GET /events (fall-through to the normal 400 path otherwise); `sse_cap_full_answers_503_through_the_real_listener` drives the REAL serve() listener and asserts the honest 503 JSON on a cap-full hub (no slot consumed).
- R2 C-round blockers (urn:ump:4ox2h4gsydux6iarj27p6pnntrl7ktpzcaxlhawxlg66h5kpf3lq) FIXED:
  1. peek_sse_path's peek window is now GROWING (1024 -> 8192): a fixed window could never see past its own end, so ANY request whose headers exceeded 1024 bytes spun the peek loop forever — a hot-path wedge for the normal POST path too. Regression test `oversized_request_headers_still_complete_peek_and_post`: GET /events with 2048 bytes of headers completes the full SSE handshake; a POST with 2048 bytes of headers gets its JSON-RPC served (HTTP 200).
  2. status_text gained 404 => "Not Found" and 503 => "Service Unavailable" (previously "HTTP/1.1 503 OK").
  3. BRIEF updated to the round-3 test count; the full suite run below is recorded RAW in the bundle.

## Round-3 final state
- Assertion fix: the oversized-header POST test now parses the served envelope (`"result":5`) instead of expecting an echoed method name.
- FINAL parent-verified full suite: `cargo test --lib` → `test result: ok. 663 passed; 0 failed; 1 ignored; finished in 102.54s` (655 base + 8 this lane: original 6 + sse_cap_full_answers_503_through_the_real_listener + oversized_request_headers_still_complete_peek_and_post). Raw runs: full-suite-r3.log (earlier run also captured) + rpc-suite-r3.log (99/0 rpc-module subset).
- Note: one earlier run on this lane's OLD base (be3cd3c) showed the pre-existing shared-port flake in in_process_node_answers_rpc — that test's crash-loop section was extracted + isolated on MASTER in 8df9447 (judge B urn:ump:uzynivob…), which this lane merges into; the flake is base-historic, not part of this lane's diff.
