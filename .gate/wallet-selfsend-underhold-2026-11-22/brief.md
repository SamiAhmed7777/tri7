# Brief — item 38 WALLET-SELFSEND-UNDERHOLD (fleet code fix, src/node_embedded.rs only)

## The live incident (DNS2, 2026-11-22 11:16–11:35Z, custody INTACT, view-only)
An EXTERNALLY-SIGNED self-send tx 6c3af322…4881 (4 wallet coin inputs → vout0
100.0 TRI to the wallet's OWN fresh address TRsxnwXeTvbdvwjj61TPLxmePqWHkfaagH
+ vout1 15.691664 TRI change to the funded address TVVcEE95o) landed canonical
in DNS2's own mint h2217415 (11:29:39Z). Afterward: listunspent rows at
TRsxnwXe = 0, getreceivedbyaddress = 0.0, wallet_utxos.json lacks the address,
plainbal fell 5527.55 → 5427.69 (≈ exactly the uncredited output) — across TWO
full post-tx AUTOHEAL rebuilds — while gettxout shows the coin canonical and
dumpprivkey/validateaddress (fresh wallet-file reads) say the address is a real
keystore address (ismine=true). The addrindex is correct (getaddressbalance
100.0, index_complete=true). Staking stayed healthy throughout.

## Root cause — VERIFIED ON THE LIVE WALLET FILE (not inferred)
The wallet live view's key set is the BOOT-TIME `read_wallet` snapshot
(`start_keys` at main_args:14369 → cloned into every `WalletLive::seed`). The
daemon booted 2026-11-22 01:58:48Z. A direct SQLite probe of
/var/lib/tri7/wallet.dat (read-only copy) shows the TRsxnwXe key row is the
NEWEST row in the whole table (rowid 1382701 = max of 63074), keymeta birth
time 2026-11-22 11:13:04Z — imported 9h14m AFTER boot, 16 min before the tx
funded it. `apply_block` credits ALL outputs into `wl.live` unconditionally,
so the 100 TRI coin sat in the live map the whole time; the ONLY thing missing
was the key: `WalletLive::owns()` (payload match against `wl.wallet.keys`)
returned false, so balance/listunspent/candidates/repair_diff all ignored it.
The change vout (pre-boot key) credited fine — which is why plainbal fell by
exactly the uncredited output. Nothing in the runtime ever re-reads the wallet
file: `dumpprivkey` reads it fresh per call (hence ismine=true) while the live
view, AUTOHEAL, and repairwallet all clone the stale boot-time set.

## The fix (3 pieces, all in src/node_embedded.rs)
1. `merge_wallet_keys(wallet, fresh)` — merge-ONLY by pubkey into the in-memory
   keystore: never removes or overwrites (in-memory entries carry runtime
   unlock state — walletpassphrase-filled secrets — a file re-read must not
   clobber); HashSet::insert is the single gate, so duplicates within the file
   read itself also cannot double-add. Returns the added keys.
2. `refresh_keys_from_file(path)` — a fresh `read_wallet`; `None` = unreadable
   (no path / missing file / ENCRYPTED container), callers skip. Encrypted
   wallets are never touched: their keys are unlock-flow managed
   (walletpassphrase fills secrets in the existing entries; walletlock zeroes
   them) and read_wallet refuses encrypted containers anyway — belt and braces.
3. Three wiring points:
   a. A `keystore-refresh` background thread (every KEYSTORE_REFRESH_SECS=120,
      first check ~5s after boot once the wallet-seed thread's view exists —
      merging before that swap would be lost). Reads the file OUTSIDE the wl
      lock, merges under a short lock, and logs loudly on additions:
      "wallet keystore refresh: +N key(s) … ownership credited in place".
      Because `apply_block` already put the coin in `wl.live`, merging the key
      credits the coin IN PLACE — no rebuild, no restart. Staking candidates
      include the coin immediately (it is canonically owned; staking it is
      correct behavior).
   b. `autoheal_stale_live_view` gains a `wallet_path` param and refreshes
      BEFORE the replay, so the rebuilt view never bakes in a stale keystore
      (guarded `!wl.wallet.encrypted && !wl.locked`). Existing call sites pass
      `None` → byte-identical prior behavior (all pre-existing tests pass).
   c. The `repairwallet` RPC arm refreshes before its replay (same guard), with
      its own loud "+N key(s)" line.

## Regression tests (4 new, all green; full suite result in evidence.txt)
- `selfsend_credits_both_vouts_once_the_post_boot_key_arrives` — THE INCIDENT
  replay (item bar ii): a real 5-block store whose h2 carries coinbase +
  coinstake-shaped tx + the self-send (vout0 100*COIN P2PKH to fresh key B,
  vout1 15.691664 change P2PK to funded key A); boot keys = A only → fresh
  vout unowned, balance == change only; a REAL SQLite wallet file (bootstrap +
  WalletHandle::add_key) holds key B; merge → owns(fresh)=true, balance ==
  fresh+change ("wallet == addrindex to the coin"), candidates include the
  fresh coin, and a fresh `WalletLive::seed` with the refreshed keys credits
  both vouts with `repair_diff` count 0.
- `autoheal_rebuild_refreshes_keys_from_the_wallet_file_first` — the full
  AUTOHEAL path with a raced-phantom trigger and the REAL file: refresh runs
  before the replay, the swap removes the phantom, and the swapped view OWNS
  the fresh vout (attempts == 1).
- `keystore_refresh_merge_is_additive_and_idempotent` — merge-only discipline:
  runtime unlock state on existing keys untouched; re-merges add nothing.
- `keystore_refresh_skips_unreadable_wallet_files` — None path, missing file,
  and an encrypted container (mkey row) all skip.

## Scope & risk
- Fleet binaries: DNS2/DNS3/Pi all run follow_tip from this tree — the fix is
  one file, additive, and the only behavior change on the happy path is a
  ~once-per-2-min read-only file scan on a dedicated thread (read happens
  OUTSIDE the live-view lock; the merge hold is a Vec extend).
- No changes to the pair/snapshot path (balances.json/wallet_utxos.json):
  the pair builder already re-reads the wallet file per build and rebuilds on
  its own 6h cadence — the live view was the only never-refreshing consumer.
- The operator note on the tx's authorization (externally signed,
  sent_txs.json empty, key imported 11:13Z by local tooling) remains flagged
  to the operator; this fix is the code answer to the VIEW defect either way.

## Post-deploy live-verification bars (DNS2)
listunspent rows at TRsxnwXe = 1 (100.0), getreceivedbyaddress = 100.0 after a
pair rebuild, plainbal back ≈ +100 TRI, wallet == addrindex to the coin (the
item-31 battery), staking unregressed, zero new STALE/AUTOHEAL lines of the
false class; Pi + DNS3 sweep-verified with the same battery.
