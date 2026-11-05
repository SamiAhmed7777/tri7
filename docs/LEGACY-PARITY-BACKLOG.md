# Legacy → tri7 parity backlog

Generated 2026-11-05 from source, not from memory.

**Evidence base**
- Legacy RPC surface: `trianglesrpc.cpp` `vRPCCommands` — **93 methods** extracted by
  parser (`/tmp/legacy_rpc.json`).
- tri7 RPC surface: `NATIVE_METHODS ∪ OPS_METHODS ∪ PROXY_FALLBACK_METHODS` in
  `src/rpc.rs` — **46 native**, of which **31 of the legacy 93 are covered**.
- Legacy GUI surface: `src/qt/*.cpp` + `src/qt/forms/*.ui` (19 dialogs/pages).
- tri7 GUI surface: `fn page_*` in `src/gui/app.rs` (11 pages).
- Live probes against DNS2 at :19112 for every "missing" claim below (all 12
  sampled returned `-32601`).

Legend: `[x]` shipped and verified · `[~]` partial · `[ ]` missing · `[–]` deliberately
out of scope (with reason).

---

## Shipped this session (for the record)

- [x] **`getnewaddress [label]`** — HD wallets derive m/44'/2222'/0'/0/N (seed
      still restores the address); non-HD wallets get a random key via add_key.
      Proven on a wallet copy: 248 -> 249 keys, key present after reopen.
- [x] **`backupwallet <dest>`** — copies the wallet, refuses an existing
      destination (legacy rule).
- [x] **`sendtoaddress <addr> <amount> [dryrun]`** — builds and signs with the
      GUI's builder, then relays through the same accept+announce path as
      sendrawtransaction. `dryrun` (non-legacy) builds and signs WITHOUT
      relaying so the path can be proven on a live wallet. Verified on DNS2:
      195-byte tx, fee 100 sats, change 30,454,219 sats, not relayed.
- [x] **`dumpprivkey` / `dumpwallet` / `importwallet` / `importprivkey` /
      `checkwallet`** — thin RPC wrappers over the existing keytool functions
      (the same ones `wallet_keytool` uses). Verified on a wallet COPY:
      dumpwallet wrote 249 keys, importwallet into an identical copy reported
      249 skipped / 0 failed (idempotent), checkwallet returned
      integrity ok, bad_keys 0. Live on DNS2: dumpprivkey returns a WIF,
      checkwallet reports 248 keys / 63,026 rows / integrity ok.
      importprivkey refuses an uncompressed WIF because tri7 stores compressed
      rows only and the address would differ from the source wallet.
- [x] **`listtransactions [account, count, from]`** and
      **`listsinceblock [blockhash]`** — paginated history in legacy record
      shape (category receive/send, confirmations, blockhash, blocktime).
      listtransactions windows newest-first then returns ascending, matching
      legacy order; unit-tested with a fixed 4-row history.

- [x] **`gettransaction`** — was in the proxy-fallback list with no handler, so it
      errored on any node without a legacy daemon. Now native and wallet-scoped:
      rebuilds `details` from output/input ownership against the wallet, so a
      stake block reports `+26.886029 receive / -26.881169 send → amount 0.00486`.
      Verified on DNS2 at h2205979.
- [x] **`getrawmempool`, `getmempoolinfo`, `getnetworkhashps`** — same class of
      bug (advertised, unimplemented, dead proxy); all three now native.
- [x] **Every method in `PROXY_FALLBACK_METHODS` has a native handler.** The proxy
      is now only a fallback for a native *miss* (index warming), never the sole
      implementation of an advertised verb.

## P0 — wallet cannot be used as a wallet without these

- [ ] **`getnewaddress`** — legacy `rpcwallet.cpp`. tri7 GUI creates keys locally,
      the RPC does not exist. Any external tool (or a second wallet UI) cannot mint
      an address.
- [ ] **`sendtoaddress` / `sendmany` / `sendfrom` / `move`** — the whole
      programmatic spend path. GUI sends fine (its own tx builder), RPC cannot.
      `move`/`sendfrom` are legacy account-model verbs: port `sendtoaddress` +
      `sendmany`, mark `move`/`sendfrom` `[–]`.
- [ ] **`signmessage` / `verifymessage`** — `verifymessage` is native; signing is
      implemented in `gui::state` for the GUI page but **not exposed on RPC**.
- [ ] **`backupwallet`** — copy `wallet.dat` to a caller path. Trivial and it is the
      one operation a holder must be able to script before touching anything.
- [ ] **`listtransactions` / `listsinceblock`** — history by account/address/count
      and "everything since block N". tri7 has `listwallettx` (full dump) but no
      windowed, paginated form; every integration expects these two.
- [ ] **`getaddressbalance` / `getaddressutxos` / `getaddresstxids`** — the
      address-index family. IMPORTANT: legacy only serves these when started
      with `-addressindex=1`, and otherwise throws
      "Address index not enabled. Start with -addressindex=1"
      (`rpcblockchain.cpp:464`). So parity means an opt-in chain-wide address
      index, not a wallet-scoped shortcut: it must cover foreign addresses too.
      A wallet-scoped implementation would answer differently from legacy and
      is therefore not acceptable.
- [ ] **`gettxoutsetinfo`** — chain state summary; cheap to compute from the index.

## P1 — parity that users will notice

### RPC
- [ ] **`listreceivedbyaddress` / `listreceivedbyaccount` / `getreceivedbyaccount`**
      — `getreceivedbyaddress` is native; the list forms are not.
- [ ] **`listaddressgroupings`** — grouping by address book / account.
- [ ] **`createrawtransaction` / `decoderawtransaction` / `decodescript` /
      `signrawtransaction`** — offline signing workflow.
- [ ] **`keypoolrefill`** — keep N addresses ready ahead of use.
- [ ] **`settxfee` / `reservebalance`** — fee policy and staking reserve.
      `reservebalance` is also a GUI Options field currently disabled.
- [ ] **`checkwallet` / `repairwallet` / `resendtx`** — self-repair verbs. Given
      this session's history (index gaps, history wipe), these are not cosmetic.
- [ ] **`getcheckpoint`** — the checkpoint the node is trusting.
- [ ] **`makekeypair`** — deterministic key generation helper.
- [ ] **`stop`** — graceful shutdown; currently only systemd/taskkill.
- [ ] **`sendalert`** — legacy alert broadcast. `[–]` if alerts are retired.

### Accounts model
- [–] **`getaccountaddress`, `setaccount`, `getaccount`, `getaddressesbyaccount`,
      `listaccounts`** — the legacy "account" abstraction. tri7 uses labels.
      Porting it would import a model the new UI does not have. Decide explicitly.

### Mining (PoW)
- [–] **`getwork`, `getworkex`, `getblocktemplate`, `submitblock`** — tri7 is
      proof-of-stake; there is no PoW miner path. `getmininginfo` and
      `getnetworkhashps` are served for compatibility.

### SMSG (encrypted messaging)
Legacy names → tri7 equivalents (renames, not gaps — verify each):
- [x] `smsgsend` → `smsgsend`
- [x] `smsginbox` → `smsginbox`
- [x] `smsgoutbox` → `smsgoutbox`
- [~] `smsglocalkeys` → `smsgkeys` + `smsglocalinfo`
- [ ] `smsgenable` / `smsgdisable` — no runtime enable/disable
- [ ] `smsgoptions` — per-message options
- [ ] `smsgscanchain` / `smsgscanbuckets` — rescan a bucket range
- [ ] `smsgaddkey` / `smsggetpubkey` — add a peer key / fetch a public key
- [ ] `smsgsendanon` — anonymous send
- [ ] `smsgbuckets` / `smsgbroadcast` — bucket enumeration / broadcast

### GUI
- [x] Overview, Transactions, Send, Receive (+URI/QR), Address Book, Sign, Verify,
      Messages, Staking, Explorer, Options — 11 pages
- [x] Coin Control (collapsible card, `app.rs:2505`)
- [x] Per-row transaction detail (`tx_details`)
- [x] Tor/I2P address drawer (single implementation on Receive; status-bar handle
      jumps to it)
- [x] Close behaviour: taskbar / tray / quit (Options → Display)
- [ ] **CSV export** of the transaction list (`csvmodelwriter.cpp`). `grep -i csv`
      in `src/gui` returns **0 hits**.
- [ ] **Wallet encryption UI** (`askpassphrasedialog`, `walletpassphrase`,
      `encryptwallet`). GUI has 21 mentions of "Encrypt" but the node only reports
      "Unencrypted"; there is no passphrase path.
- [ ] **Start Triangles on login** — Options row currently disabled.
- [ ] **Detach databases** — Options row currently disabled.
- [ ] **Staking reserve field** — disabled in Options; pairs with `reservebalance`.
- [ ] **Real tray icon** — the tray option currently minimises instead of showing
      an icon (`tray-icon` crate not wired).

## P2 — packaging, transports, operations

- [~] **Tor** — onion identity live on DNS2 and sami-pc
      (`tuu7h65jykj327um6jfcecqmgyq6mjn3psiay5mtald2olsswk4jcrad.onion`), outbound
      via 127.0.0.1:9050 on DNS2/Samihost.
- [ ] **I2P** — `getlocalinfo` reports `i2p: null` on sami-pc; no i2pd listening on
      4447. Needs i2pd installed + destination published + the address drawer
      showing it (drawer already renders the row).
- [ ] **Windows installer refresh** for the current build (logo, close options,
      drawer handle).
- [ ] **Laptop install verified end-to-end** — 7.0.1 installed, but 0 connections
      and no RPC when last checked; "normal shortcut" still outstanding.
- [ ] **Real tray icon** (see P1 GUI).
- [ ] **Auto-updater** — built and graded; has not been exercised against a real
      newer release.

## Verification debt (not features, but parity of trust)

- [ ] **Click-audit every control** (`computer_use`) — blocked because the desktop
      session is not reachable from the Hermes host; needs a session-1 agent or
      Sami's eyes.
- [x] **`consistency` tool** — validates history vs live (dates, duplicates,
      height resolution, live-UTXO subset). Last sami-pc run: CONSISTENT.
- [x] **`store_gapfix`** — fills missing heights, hash9-verified against tipstate;
      used to fill 33 heights on sami-pc.
- [ ] **`checkwallet` equivalent in CI** — run `consistency` after every release.

---

## Counted status

| Area | Legacy | tri7 | Covered |
|---|---|---|---|
| RPC methods | 93 | 46 native | 31 (33%) |
| GUI pages/dialogs | 19 | 11 pages | ~13 (68%) |
| Transports | clearnet + Tor + I2P | clearnet + Tor | partial |

**Not counted as parity gaps** (intentional): PoW mining verbs, the legacy account
model, `sendalert`.

## Suggested order

1. `getnewaddress`, `sendtoaddress`, `sendmany`, `listtransactions`,
   `listsinceblock` — a wallet that cannot be scripted is not a wallet.
2. `backupwallet`, `dumpprivkey`/`importprivkey` — the holder's exit door.
3. Address-index family (`getaddress*`) — unlocks explorers and processors.
4. Wallet encryption + `reservebalance` — the two Options rows still disabled.
5. I2P, tray icon, installer refresh, laptop verification.
6. SMSG renames, then the genuinely missing smsg verbs.
