# The wallet's window must never wait on the node

*Scope*: one code change, four files in `/root/tri7` (branch `master`, on top of `50bab5df27ced8f6e37950adf76dd7c65bdcd26f`).
*Asked for* (Sami, 2026-11-27): "When running the GUI no one should ever Notice or care about the daemon."
*Kind*: code. *Round*: 2 — a delta re-grade after the stand-in judge's round-1 polish suggestions (§7).
*Not in this package*: any deployment to sami-pc (separate gate), any edit to `src/node_embedded.rs`.

## 1. What is wrong today (measured, not inferred)

On Sami's laptop the deployed wallet (exe `a06ca5ee…`, built 2026-11-23) shows no window at all while its node
syncs and stakes. `ev_before_pc.txt` and `ev_stack_rootcause.txt` hold the raw data:

* 16.7 h after launch the process had used 59,754 s of CPU — 99 % of one core since it started.
* 42,275 s of that is the **window's own thread**.
* 60 samples of that thread: **40 are parked on the wallet live-view mutex** inside
  `node_embedded::OpsAdapter::call`, reached from `Tri7App::overview_wallet_actions` (render code);
  **11 are inside `hash160`** under `rpc::native_call` → `rpc::wallet_total` → `WalletLive::owns`.
* The GUI wrote one refresh in 15 minutes; `MainWindowHandle` = 0.

Cause, in the code: the 2026-11-23 change routed every GUI call through `node_embedded::local_rpc`,
the **in-process** dispatcher, so each answer is computed on the render thread; some answers cost seconds
(a balance walk that hashes every live output) and all of them line up behind a mutex the node thread
holds while it seeds. Separately, `Tri7App::update` ran the whole bootstrap (tor, i2p, node spawn, snapshot
build) inside its first frame. A render thread that cannot return cannot paint.

## 2. The change (`4 files changed, 754 insertions(+), 46 deletions(-)`)

**(a) `src/gui/node_supervisor.rs` + `src/gui/app.rs` — the supervisor leaves the UI thread.**
`NodeWorker` owns the `EmbeddedNode` on its own thread and re-arms it about every 15 s, the cadence the
UI tick had. The UI only reads `NodeStatusView { tor, i2p, running, restarts, last_error }` (a clone
under a short mutex). `ordered_exit` calls `stop_and_join(5 s)`: the worker stops the node, then the
children are reaped — same order as before. A worker thread that cannot start surfaces as `last_error`
instead of a node that silently never starts; a worker that does not stop inside the deadline is logged
(`supervisor: node worker did not stop in time …`), not silently detached.

**(b) `src/gui/rpc_client.rs` — the UI thread never runs node code for long.**
Embedded-mode `rpc_call` keeps its signature and hands the dispatcher to a short-lived worker:

* **READ** — waits at most 400 ms for a fresh answer; if the node is busy it returns the previous answer
  and the worker keeps going (the next frame shows the new one). With nothing to show yet it waits at most
  1.5 s — the socket path's own connect budget — then says the wallet is still starting.
  The same verb asked twice within 300 ms gets the same answer (three balance cards ask per frame);
  an error is never replayed.
* **ACTION** (`sendrawtransaction`, `staking`, `walletpassphrase`, `encryptwallet`, messaging writes …) —
  never answered from a cache, never abandoned: it runs exactly once and the caller waits up to 30 s for
  **its own** result. A double-click joins the call already on its way rather than sending twice.
  When an action finishes, every finished READ answer is forgotten — the wallet changed.
* **Bounded** — at most 8 node workers at once. A caller takes a slot first and gives it back if the pool
  is already full (`fetch_add` then compare), so two callers cannot both pass a check and overshoot.
  Over the cap a READ shows what it has; an ACTION runs inline. At most 48 answers kept; every flight
  carries an id so a worker that dies (panic) releases only its own callers, with an error, never a later
  call's.
* **Keyed exactly** — an answer is stored under the method plus the exact parameter text. Round 1 kept a
  64-bit digest of the parameters; two different calls sharing a digest would have been handed each
  other's answers. In a wallet that has to be impossible, not improbable.
* `local_rpc` was already being called concurrently from the loopback RPC listener ("one OpsAdapter, two
  surfaces"), so running it from one more thread is within its existing contract.

**(c) `src/gui/tests.rs`** — fixes a test module that does not compile at `50bab5d` (the passphrase-dialog
`state()` helper returned `(AppState, TestDir)` while every caller binds the guard first) and keeps one
`NodeWorker` test: `start` returns at once and `stop_and_join` honours its deadline.

## 3. What this does NOT do (said plainly)

* It does not make the node's answers cheaper. `getwalletinfo` still hashes every live output; that work
  now runs off the window thread, so the window paints, but the CPU is still spent. `node_embedded.rs` is
  being edited by another work item, so the provider itself is deliberately left alone here.
* The wording on screen ("daemon", "tri7d", "RPC") is unchanged; it is a separate, string-only change.
* Nothing is deployed by this change. The proof on the real laptop is a later gate: CI artifact, hash-gated
  swap, dated backup, rollback — the same procedure used on 2026-11-26.

## 4. How it was verified — on the exact tree that lands

The four files were copied into a separate worktree at `50bab5d` with **no other edits present**, and built
there with a fresh target directory. Transcript: `ev_isolated_check.txt`.

### CMD: (worktree = 50bab5d + the four files) cargo check --features gui --bin tri7_wallet_gui
Finished `dev` profile [unoptimized + debuginfo] target(s) in 26.32s
CHECK_EXIT=0
error lines: 0

### CMD: (same worktree) cargo test --features gui --lib -- rpc_client:: gui::tests::node_worker gui::tests::passphrase
test gui::rpc_client::tests::humanize_error_leads_with_plain_language ... ok
test gui::rpc_client::tests::different_calls_keep_their_own_answers ... ok
test gui::rpc_client::tests::action_always_runs_and_waits_for_its_own_result ... ok
test gui::rpc_client::tests::non_loopback_rpc_addr_is_refused_before_io ... ok
test gui::rpc_client::tests::a_panicking_worker_releases_its_callers ... ok
test gui::rpc_client::tests::read_shows_the_previous_answer_instead_of_waiting ... ok
test gui::rpc_client::tests::rpc_call_parses_canned_response_and_enforces_allowlist ... ok
test gui::rpc_client::tests::rpc_call_routes_in_process_when_the_embedded_dispatcher_is_live ... ok
test gui::rpc_client::tests::rpc_call_sends_the_real_bearer_token ... ok
test gui::rpc_client::tests::rpc_call_unreachable_is_clean_error ... ok
test gui::tests::passphrase_dialog::change_needs_both_old_and_new ... ok
test gui::tests::passphrase_dialog::encrypt_requires_matching_new_passphrase ... ok
test gui::tests::passphrase_dialog::no_dialog_pending_is_an_error_not_a_silent_noop ... ok
test gui::tests::passphrase_dialog::decrypt_needs_only_the_current_passphrase ... ok
test gui::tests::passphrase_dialog::encrypt_refuses_short_passphrase ... ok
test gui::tests::passphrase_dialog::unlock_needs_only_the_current_passphrase_and_a_duration ... ok
test gui::tests::node_worker_start_returns_immediately_and_stops_promptly ... ok
test gui::rpc_client::tests::worker_pool_is_bounded ... ok
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 980 filtered out; finished in 24.34s
TEST_EXIT=0

Tests that drive the real engine with a stand-in dispatcher, so the timings are deterministic:
`read_shows_the_previous_answer_instead_of_waiting`, `action_always_runs_and_waits_for_its_own_result`,
`a_panicking_worker_releases_its_callers`, `worker_pool_is_bounded`,
`different_calls_keep_their_own_answers`. They share one lock with the other `rpc_call` tests because the
capability token, the dispatcher stub and the answer store are process-wide.
The existing fused30 routing test (`rpc_call_routes_in_process_when_the_embedded_dispatcher_is_live`) still
passes unchanged in what it asserts: a live dispatcher answers without a socket and error envelopes are
humanized; it now clears the answer store between its two halves, because a READ asked twice inside
300 ms is answered from the first result by design.

### CMD: (same worktree) cargo test --features gui --lib
test result: ok. 996 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 124.03s
FULL_EXIT=0

For context, the main working tree `/root/tri7` (which also carries another agent's uncommitted
`src/node_embedded.rs`, four tests more) was run before the round-2 polish fixes: 999 passed, 0 failed,
and `cargo test --features gui --doc` 0 passed, 0 failed. That transcript is `ev_tests.txt`. The commit
stages exactly the four files of this change, so the numbers above are the ones that describe what lands.

### CMD: git diff --stat -- src/gui/app.rs src/gui/node_supervisor.rs src/gui/rpc_client.rs src/gui/tests.rs
 src/gui/app.rs             |  75 ++++---
 src/gui/node_supervisor.rs | 149 ++++++++++++-
 src/gui/rpc_client.rs      | 528 ++++++++++++++++++++++++++++++++++++++++++++-
 src/gui/tests.rs           |  48 ++++-
 4 files changed, 754 insertions(+), 46 deletions(-)

### CMD: git branch --show-current ; git rev-parse HEAD
master
50bab5df27ced8f6e37950adf76dd7c65bdcd26f

### CMD: secret scan of ev_change.diff
credential-shaped matches: 0 (AWS keys, sk- keys, private-key blocks, Slack/GitHub tokens, bearer tokens)

Files in this package: `BRIEF.md`, `ev_change.diff` (the exact reviewed diff), `ev_round2_delta.txt`
(what changed since round 1, with grep proof), `ev_round1_change.diff`, `ev_stack_rootcause.txt`,
`ev_before_pc.txt`, `ev_tests.txt`, `ev_isolated_check.txt`, `ev_git.txt`, `ev_secret_scan.txt`.

## 5. If it goes wrong

The change is confined to the GUI crate's call path; `rpc_call` keeps its signature and its socket path
untouched. An ACTION is never replayed from a cache, so no path can send twice. Reverting the commit
restores the previous behaviour exactly (a window that waits — which is the bug).

## 6. Ask

Grade the code change for commit. On a pass: stage exactly the four files, commit with `[grade=X]`.

Answer with a single JSON object on the last line and nothing after it:
`{"codex_grade": "A|B|C|D|F", "reasons": [...], "blocking_issues": [...], "polish_suggestions": [...]}`.

## 7. Judge history of this package

* **Primary gate, round 1** — `judge-artifact.sh` returned `F` with the single blocker
  `malformed JSON from Codex` (`urn:ump:nwx4hqjjs5xhr2p6vigburz47aqk6amb4ifgogqgyfhzpp3oa72q`): an
  infrastructure failure, not a grade on this change. A direct probe of the CLI answered
  `ERROR: You've hit your usage limit ... try again at Nov 29th, 2026 1:40 PM`.
* **Stand-in (`--reason quota`), round 1** — **B**, zero blockers,
  `urn:ump:ododqs3zeabqucdgnleks57n6svo6pp6uymffhwnye2hqf56ix7q`, with four polish suggestions.
* **Round 2 (this submission)** — three of the four were applied, and each is checked against the live
  files in `ev_round2_delta.txt`:
  1. *WORKER_CAP check-and-increment is not atomic* → the slot is now taken first with `fetch_add` and
     returned if the pool is full (`start_flight`, `src/gui/rpc_client.rs`).
  2. *A digest key can collide and serve a wrong answer* → the key is now the exact parameter text
     (`cache_key`, `src/gui/rpc_client.rs`), plus the test `different_calls_keep_their_own_answers`.
  3. *`stop_and_join` detaches silently on timeout* → it now logs that the worker is still stopping
     (`src/gui/node_supervisor.rs`).
  4. *A queue-based persistent pool instead of a thread per call* → **not applied**. It is a design
     preference, not a defect: calls are coalesced (the same ask within 300 ms does not start a worker)
     and capped at 8, so a frame starts at most a handful of short-lived threads; a persistent pool
     would add a queue, a shutdown path and state to test for no user-visible gain here. Recorded as a
     possible follow-up.

  This round supersedes `urn:ump:ododqs3zeabqucdgnleks57n6svo6pp6uymffhwnye2hqf56ix7q`; it is a delta
  re-grade — the three hunks in `ev_round2_delta.txt` are the only difference from the bytes that earned
  the B.
