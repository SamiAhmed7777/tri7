# Judge brief: tri7 lane-singleexe — ONE user artifact: the wallet IS the node

## OPERATOR DIRECTIVE (2026-11-10, verbatim intent)
"That follow chip software and the other EXE files should be combined into
the main software and it should run like a normal person would run the
wallet." One file. tri7_wallet_gui.exe IS the wallet, the node, the staker.

## WHAT THIS ARTIFACT IS (+14,266/-14,017 across 8 files)
1. **src/node_embedded.rs (NEW, 14,004 lines)**: src/bin/follow_tip.rs moved
   VERBATIM (13,991 lines + 3 new tests); the only deltas are the briefed
   ones: fn main() → pub fn main_args(argv) -> i32; pub fn run_embedded(argv)
   -> Result<i32,String> (catch_unwind + NodeExit(i32) panic-payload exit
   trap); all 5 process::exit sites funneled through node_exit() — the
   external path is byte-identical; log() tees to the supervisor's node.log.
2. **src/bin/follow_tip.rs**: 9-line shim (fleet/dev external workflow
   unchanged; verified: bad flag→2, --status→3).
3. **src/lib.rs**: pub mod node_embedded + extern crate self as tri7_core
   (the moved file's ~300 tri7_core:: paths resolve in-crate).
4. **node_supervisor.rs (+309/-x)**: Command::new(follow_tip) spawn DELETED;
   the node runs IN-PROCESS on a dedicated thread with the IDENTICAL argv the
   old Command built; attach mode (rpc_addr configured) is liveness-check-only
   and NEVER starts anything in-process; restart bookkeeping preserved;
   balances.exe snapshot spawn untouched.
5. **CI/NSIS**: windows job user payloads = setup.exe + gui-portable.exe +
   SHA256SUMS (tri7d.exe portable dropped; dev-tool bins still build);
   tri7-wallet.nsi installs ONE exe (+ legacy .lnk cleanup for pre-weld
   installs); tri7-verify.sh greps repointed to src/node_embedded.rs.
6. **gui/app.rs**: 1 line — passes rpc_addr.is_some() as the attach flag.

## THE WELD'S BEHAVIOR
- Normal person: double-click tri7_wallet_gui.exe → the node starts
  in-process → staking runs inside the same process → ONE file in the folder.
- Fleet/advanced: rpc_addr configured → attach to the external daemon,
  in-process node never starts — the 5-node fleet is untouched.

## TESTS (652 = 569 master base + 80 moved follow_tip tests + 3 new)
- in_process_node_answers_rpc (stub runner serves the real JSON-RPC envelope;
  RPC-wait → Ok(true), running() true)
- attach_external_never_starts_in_process (dead port → Ok(false), runner
  invoked 0×)
- exit_trap_returns_code (Ok(EXIT_USAGE), EMBEDDED guard restored)
Parent re-ran the full suite + all three by name (see tests-singleexe.txt).

## GUI-FEATURE COMPILE
cargo check --bin tri7_wallet_gui --features gui: 0 errors (command + result
in tests-singleexe.txt).

## BINARY SIZE (honest, same command + profile, linux release LTO)
base e42c79b: 18,442,752 B → post-weld: 20,716,072 B = +2.27 MB (+12.3%)
(the node rides along cheaply — it already shares the lib's codegen).

## FILES (exactly what is in this bundle)
- brief.md — this brief (FILES section regenerated to match)
- singleexe.diff — git diff master..HEAD (the full weld + merged state)
- node_embedded-full.rs — the 14,004-line moved module (verbatim move + 3 new tests)
- updater-merged-tree.rs — src/updater.rs AS MERGED (the identity/lifecycle wrapper builder this tree ships)
- wrapper-final.bat — the wrapper dumped from THIS tree (TRI7_DUMP_WRAPPER=1)
- wrapper-traced.bat — mechanically derived (make-traced.py, in bundle); non-breadcrumb lines identical
- wrapper-spaces.bat — the Spaces-path variant
- hashes.txt — sha256 of all three wrappers, hashed from these files
- tests-full.log — COMPLETE cargo test --lib output (652 passed = master base 569 + 80 moved follow_tip tests + 3 new)
- gui-feature-check.log — COMPLETE cargo check --bin tri7_wallet_gui --features gui output
- tests-singleexe.txt — evidence digest (commands, result lines, lifecycle greps)
- live-windows-verification.txt — the real-Windows run of these exact bytes
- raw-final-run.txt — raw RPC + cleanup output from the live run
- make-traced.py — the mechanical traced-variant injector
