# Brief: tri7 KEYPOOLREFILL (workqueue item 5) — legacy keypool verb + top-up wiring

## Artifact
Repo /root/tri7 (master, parent b0e9684), diff in `change.diff`
(5 files, ~1100 lines: wallet.rs +530 incl. tests, follow_tip.rs +272,
rpc.rs +60, two doc lines). Deliverable: the legacy `keypoolrefill [size]`
RPC verb, getnewaddress served from the pool, a background top-up
maintainer, and honest keypool fields in getwalletinfo — closing the
LEGACY-PARITY-BACKLOG P1 "keypoolrefill — the legacy pool is imported but
not topped up" item.

## Ground truth the implementation is byte-matched to
The live DNS2 wallet carries 116 imported legacy `pool` rows. One row was
decoded byte-exactly and pinned as a test fixture
(`pool_row_bytes_decode_live_ground_truth`):
- key blob   = `04` + `"pool"` + int64 LE record index (13 B)
- value      = int32 LE version (`0x004DB174` — the qt's constant, kept)
              + int64 LE nTime + CS(pubkey len) + compressed pubkey (46 B)
tri7's writer (encode_pool_keyblob / encode_pool_value) re-encodes that
fixture byte-identically, so qt-written rows load in tri7 and tri7-written
rows load in the qt. Reader tolerates malformed pool rows (skipped, never
fatal — pool membership is bookkeeping, not key material).

## Delta vs HEAD (b0e9684)
- ADDED wallet.rs `KeyPoolEntry` / `KeyPoolRefill` / `WalletKeys.keypool`
  (sorted lowest-index-first) + encoders + private parse_pool_row +
  `keypool_stats(path)` (cheap targeted SQL census: count + oldest, no
  full-file key parse).
- ADDED WalletHandle methods:
  - `pool_entries()` — fresh ordered census through the open handle.
  - `refill_keypool(target)` — HD (seed) wallets ONLY (non-HD refused with
    an explicit "no HD seed" error: an imported random-key wallet cannot be
    topped up from a phrase and we say so instead of inventing keys).
    Derives m/44'/2222'/0'/0/n via the EXISTING generate_hd_key (its graded
    atomic 4-row transaction + full reader verify are untouched — zero new
    write paths for key material), then writes the pool row in its own
    single-row atomic write_rows. A crash between the two commits leaves a
    key owned but un-pooled — harmless by construction (the phrase restores
    it; the counter never reuses an index). One full-reader verification at
    the end asserts the reader sees exactly poolsize rows. Empty label ""
    = legacy default-account parity. A pool at/above target is a no-op;
    the pool is never shrunk.
  - `pop_keypool(label)` — hands out the lowest-index entry whose `key`
    row EXISTS (import artifacts with no key row are skipped loudly: an
    address the wallet cannot spend from is worse than a live derivation).
    DELETE pool row + relabel name row in ONE transaction, post-commit
    byte-verify both. The key row itself is kept (legacy semantics —
    consuming a pool reservation never deletes key material).
- CHANGED rpc.rs: NATIVE_METHODS + OPS_METHODS += "keypoolrefill" (routed
  to the live ops engine — it writes the wallet file); WalletExtras gains
  keypool_size/keypool_oldest (None = unknown, 0 = genuinely empty);
  getwalletinfo reports the REAL census instead of hardcoded nulls
  (txcount stays null — the snapshot has no tx-count). help auto-derives.
- CHANGED follow_tip.rs:
  - `keypoolrefill [size]` arm: absent/0 → default 100, legacy 0..=100000
    range (else -8, the qt's invalid-parameter code), opens the wallet
    RW, refills, republishes the census, returns
    {derived, poolsize, keypoololdest, hdchain_next_index, target}.
  - `getnewaddress` arm: pops the pool FIRST (pooled keys are already
    phrase-derived with key rows — instant issuance, identical restore
    semantics), falls back to the existing live derivation on empty pool;
    a pool READ failure degrades to live derivation with a loud log
    (issuance never blocks); census republished after every handout; log
    line distinguishes "keypool index N" vs "derived (keypool empty)".
  - `keypool-topup` background thread (spawned once at ops start when a
    wallet is configured): cheap SQL census every 15 min → refill to 100
    only when below target; publishes the census to getwalletinfo even
    when doing nothing; unreadable/unopenable wallet → daily retry, never
    a crash or log storm; fresh seed wallets (Pi/DNS3) get topped to 100
    in the background without blocking node start.
  - Startup: immediate census publish (keypool_stats — cheap, works even
    when read_wallet refuses an encrypted wallet).
- DOCS: wallet-format.md pool row note (tri7 writes these rows, constants
  documented); LEGACY-PARITY-BACKLOG item marked [x] with the semantics.

## Verification already run (commands + results, from the reviewed tree)
- cargo check --bins --features gui: zero errors.
- cargo test --lib: 469 passed / 0 failed (464 before + 5 new:
  pool_row_bytes_decode_live_ground_truth, keypool_refill_pop_and_census_
  lifecycle, pop_skips_pool_rows_without_key_rows, refill_refuses_non_hd_
  wallet, getwalletinfo_reports_the_real_keypool_census).
- cargo test --bin follow_tip: 43 passed / 0 failed (42 + 1 new
  keypool_size_argument_legacy_semantics).
- Release cross-builds: x86_64 linux (bec37528…), aarch64 linux
  (462e41ea…), x86_64-pc-windows-gnu all-bins (788a7841…) all green.

## Known honest limitations (disclosed, by design)
- Refill is HD-only. Encrypted wallets can be READ (census) but not
  written (open_readwrite refuses) — the maintainer logs and retries daily;
  the fleet wallets are unencrypted.
- A crash between generate_hd_key and the pool-row insert leaves the key
  owned but un-pooled (disclosed in code; no loss, counter never reuses).
- Pool indices are informational (the qt's mapKeyPool key); tri7 derives
  the pool index = the key's HD index, matching the live import where they
  coincide (21..136 vs the wallet's counter — old rows below the counter
  are exactly why pop verifies the key row exists).
- getwalletinfo keypool fields stay null until the ops engine has run its
  first census (passive-without-wallet nodes answer honestly null).

## Round-1 polish taken (judge urn:ump:lywatm7w6ziysapykpqnt2ox4i77j237vjp4k5ohndb7lmz4fala, B zero blockers)
- register_pool_key now PRE-SCANS the target index and logs loudly when a
  pool row already exists there (INSERT OR REPLACE forensics — under normal
  operation the counter never reuses an index).
- Census semantics documented on keypool_stats + WalletExtras: every pool
  row counts (qt GetKeyPoolSize parity), artifacts included; artifact-skip
  is deliberately scoped to the ISSUANCE path (pop_keypool).

## Round-3 delta — LIVE-FAILURE FIX (found during fleet verification)
Deploying on DNS2 surfaced a real defect the unit tests missed: DNS2's
imported pool rows run indices 21..121 — UP TO the HD counter (121). A
refill at the counter therefore REPLACES an imported pool row (the qt's
WritePool is the same BerkeleyDB key — INSERT OR REPLACE parity) instead of
adding one, so the file count grew by 1 while the loop counted +2 → the
end-of-refill full-reader verification correctly refused with "reader sees
102, expected 103" (no data loss: the replaced import row's KEY stays owned;
the new derived key is fully pooled).
FIX: refill_keypool now RECOUNTS poolsize from the file after every register
(pool_entries) instead of blind incrementing; the loop terminates on the
real file count, so replacement-heavy imports converge and the reader
verification stays in sync. The round-2 register overwrite log was already
in place (the deployed binary predated it — rebuilt now).
NEW REGRESSION TEST: refill_recounts_when_register_replaces_an_imported_row
— injects a pool row AT the counter index (the DNS2 shape, pubkey with no
key row), refills, asserts derived=4/poolsize=4/counter=6, the artifact row
replaced, indices counter-coherent. Suite: 470 lib (was 469) / 0 failed,
follow_tip 43 / 0 failed; release rebuilds x86_64 (a3d2044f), aarch64
(378a4d52), windows-gnu (bea85c34) green. Note: HD counters on DNS2 moved
121→123 during the failed-verify run (keys 121,122 derived + pooled, owned,
phrase-restorable); the fixed binary will be redeployed and re-verified.

## Rubric focus
Byte-parity of pool rows both directions; no regression to the graded
generate_hd_key write path; pop can never hand out an unspendable address;
honesty of getwalletinfo (real numbers vs null); argument parity with the
legacy verb; tests cover decode/encode round-trip of LIVE bytes, refill
idempotence, pop ordering/relabel/artifact-skip, non-HD refusal, arg
parsing, and the RPC census wiring. Answer with a single JSON object
{codex_grade, reasons, blocking_issues, polish_suggestions} — last line,
nothing after.
