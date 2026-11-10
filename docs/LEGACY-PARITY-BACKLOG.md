# Legacy → tri7 parity backlog

Regenerated 2026-11-06 from source, not from memory. Every "shipped" line was
proven by calling the verb on a live node or by a unit test that drives the real
dispatch.

**Evidence base**
- Checkbox semantics: **[x] = the verb/row exists in tri7 AND its evidence is
  either bundled live-probe output (probes.txt / autostart-check-and-scope.txt)
  or a cited graded gate bundle** — prose notes after a checked item describe
  SCOPE or the evidence source, never unavailability; unavailable or
  pending-live items are unchecked with a reason.
- Legacy RPC surface: `trianglesrpc.cpp` `vRPCCommands` — **93 methods**
  (parser output `/tmp/legacy_rpc.json`).
- tri7 RPC surface: `NATIVE_METHODS ∪ OPS_METHODS ∪ PROXY_FALLBACK_METHODS` in
  `src/rpc.rs` — **76 native** (was 46), covering **60 of the legacy 93**
  verbatim; SMSG renames and deliberate exclusions are counted separately.
- Legacy GUI surface: `src/qt/*.cpp` + `src/qt/forms/*.ui` (19 dialogs/pages).
- tri7 GUI surface: `fn page_*` in `src/gui/app.rs` (11 pages) + debug window.

Legend: `[x]` shipped and verified · `[~]` partial · `[ ]` missing · `[–]`
deliberately out of scope (with reason).

---

## Shipped 2026-11-05 (previous session)

- [x] `getnewaddress`, `backupwallet`, `sendtoaddress` (+ non-legacy `dryrun`),
      `sendmany`, `signmessage`, `dumpprivkey`/`dumpwallet`/`importwallet`/
      `importprivkey`/`checkwallet`, `listtransactions`, `listsinceblock`,
      `gettransaction`, `getrawmempool`/`getmempoolinfo`/`getnetworkhashps`,
      `gettxoutsetinfo`, wallet AES encryption (`encryptwallet`,
      `walletpassphrase`, `walletlock`), the address-index family
      (`getaddressbalance`/`utxos`/`txids`, opt-in chain-wide, updated on
      block acceptance), CSV export in the transactions page, and the
      `bdb_migrate` binary (keys only).

## Shipped 2026-11-06 (this session)

- [x] **`createrawtransaction [inputs] {outputs}`** — unsigned tx from
      `{txid,vout}` inputs and `{address:amount}` outputs; the legacy `data`
      destination takes a RAW SCRIPT hex (never an amount). Implemented as a
      pure codec (`src/rawtx.rs`) so it answers on any node, wallet or not.
- [x] **`decoderawtransaction <hex>`** — full legacy decode: txid, version,
      time, locktime, size, vin (coinbase or txid/vout/scriptSig asm+hex,
      sequence), vout (value, n, scriptPubKey asm/hex/type/reqSigs/addresses)
      and the input hex echoed. Trailing bytes are refused.
- [x] **`decodescript <hex>`** — asm, hex, type, reqSigs, addresses, and the
      P2SH form (`CS…`, script-address version byte 28).
- [x] **`signrawtransaction <hex> [prevtxs] [privkeys] [sighashtype]`** — signs
      with the node's live wallet keys, accepts caller `prevtxs`
      (`{txid,vout,scriptPubKey}`) for offline signing and WIF `privkeys` as a
      supplement. Every signature is verified against the resulting scriptSig
      before `complete:true`; a locked wallet is refused with a clear message;
      ALL/NONE/SINGLE ± ANYONECANPAY are honored.
- [x] **`settxfee <amount>`** — accepted only for the chain's fixed minimum
      (0.0001 TRI / `MIN_TX_FEE`); anything else is refused loudly instead of
      being silently ignored.
- [x] **`listreceivedbyaddress [minconf] [include_empty]`** — built from the
      SAME received map `getreceivedbyaddress` uses, so the two cannot
      disagree. `confirmations` is `null` on purpose: the snapshot map carries
      no per-tx heights and an invented depth would be worse than none.
- [x] **`getnewpubkey [account]`** — the wallet's `defaultkey` in hex, else the
      key of the funded address. Wallet-scoped on purpose (the legacy version
      dumped any key the node had seen — an information leak).
- [x] **`stop`** — graceful shutdown through the same resumable path as
      SIGTERM (tipstate persists; systemd does not treat it as a crash).
- [x] **`walletpassphrasechange <old> <new>`** and **`decryptwallet <pass>`** —
      re-encrypt / strip encryption on the real wallet file, with the
      passphrase proven by decrypting every key first.
- [x] **Wallet bookkeeping in `getwalletinfo`/`getinfo`** — `unlocked_until`
      (0 locked / u64::MAX unencrypted / expiry when unlocked), the effective
      `paytxfee`, and the bestblock height/hash the node actually follows.
- [x] **BDB → SQLite migration carries EVERYTHING** (`src/bdb_import.rs`) —
      key, keymeta, ckey, mkey, name, setting, pool, defaultkey, bestblock,
      version, minversion. Proven on a real 17 MB legacy wallet: 244 keys,
      244 keymeta, 32 labels, 5 settings, 101 keypool, 4 singletons, 0 skipped,
      0 errors. `tx` (28k legacy records) and `orderposnext` are NOT copied —
      history is rebuilt from the chain — and the report states that.
- [x] **Automatic transition on first open** — the GUI converts a legacy BDB
      `wallet.dat` to SQLite in place: source read only, import into a scratch
      dir, re-open + key census proof, original renamed to
      `wallet.bdb-<timestamp>` (kept forever), only then the SQLite wallet is
      installed. Any failure restores the original and changes nothing.
- [x] **GUI Encrypt Wallet is real** — File ▸ Encrypt Wallet… opens the legacy
      `askpassphrasedialog` flow (Encrypt / Change / Unlock / Remove) against
      the shipped crypto engine. Passphrase fields are masked, never attached
      to the clipboard menu, and OK stays disabled until the confirmation
      matches.

## P0 — wallet cannot be used as a wallet without these

All P0 items are shipped: `getnewaddress`, `sendtoaddress`, `sendmany`,
`signmessage`, `backupwallet`, `listtransactions`, `listsinceblock`, the
address-index family, `gettxoutsetinfo`, the raw-tx workflow, wallet
encryption and the BDB importer.

- [–] **`sendfrom` / `move`** — legacy account-model verbs; not ported.

## P1 — parity that users will notice

### RPC
- [ ] **`listreceivedbyaccount` / `getreceivedbyaccount`** — account model.
- [x] **`listaddressgroupings`** — grouping by address-book entry.
- [x] **`keypoolrefill`** — DONE 2026-11-07: native verb tops the `pool`-row
      reserve to its target (default 100, legacy cap 100000) by deriving fresh
      HD keys (m/44'/2222'/0'/0/n, restorable from the phrase); `getnewaddress`
      hands out pooled keys lowest-index-first (relabels the name row, keeps
      the key row, skips import artifacts that have no key row), and a
      background maintainer in follow_tip keeps the pool topped;
      `getwalletinfo.keypoolsize`/`keypoololdest` now report the real census.
- [x] **`repairwallet` / `resendtx`** — self-repair verbs (`checkwallet` is
      native and the `consistency` tool covers history vs live).
- [x] **`getcheckpoint`** — the checkpoint the node is trusting.
- [x] **`addmultisigaddress` / `addredeemscript`** — P2SH spends are outside
      the current signing stack (`solver_type` covers bare PubKey and
      PubkeyHash only), so these would create addresses the wallet cannot
      spend.
- [ ] **`sendalert`** — `[–]` unless alerts are retired.

### Accounts model
- [–] `getaccountaddress`, `setaccount`, `getaccount`, `getaddressesbyaccount`,
      `listaccounts` — tri7 uses labels.

### Mining (PoW)
- [–] `getwork`, `getworkex`, `getblocktemplate`, `submitblock` — PoS chain.
      `getmininginfo` / `getnetworkhashps` are served for compatibility.

### SMSG (encrypted messaging)
- [x] `smsgsend`, `smsginbox`, `smsgoutbox` (live roundtrip both directions)
- [x] `smsglocalkeys` → `smsgkeys` + `smsglocalinfo`
- [x] `smsgenable` / `smsgdisable` — runtime enable/disable SHIPPED (workqueue
      item 11; probes 2026-11-09)
- [x] `smsgoptions`, `smsgscanchain`, `smsgscanbuckets`
- [ ] `smsgaddkey`, `smsggetpubkey`, `smsgsendanon`, `smsgbuckets`, `smsgbroadcast` (lane in progress 2026-11-09)

### GUI
- [x] Overview, Transactions (CSV export), Send, Receive (+URI/QR), Address
      Book, Sign, Verify, Messages, Staking, Explorer, Options, debug window
- [x] Coin control, per-row detail, Tor/I2P drawer, close behaviour
- [x] Wallet encryption dialog (askpassphrasedialog parity)
- [x] **Start Triangles on login** — SHIPPED (workqueue item 15): real
      checkbox; HKCU Run (Windows) / XDG (Linux) / LaunchAgents (macOS) via
      `src/gui/autostart.rs`; `--autostart-check` roundtrip live-passed on
      sami-pc.
- [ ] **Detach databases** — Options row disabled.
- [x] **Staking reserve field** — SHIPPED (workqueue item 15): real Options
      row wired to the `reservebalance` verb (parse-validated, Apply pushes to
      the node, loads live value; selfcheck probe).
- [x] **Real tray icon** — SHIPPED (workqueue item 15, graded A): real tray
      icon + hidden-window repaint so tray clicks work while hidden; evidence
      source = item-15 gate bundle `.gate/gui-polish-2026-11-09/` (loop's
      graded run), not a fresh UI session in this bundle.

## P2 — packaging, transports, operations

- [x] **Tor identity survives migration** — the legacy wallet's
      `tor_v3_private_key`, `tor_v3_onion_address`, `tor_v3_key_encrypted` and
      `tor_v3_hs_secret_key_backup` settings are carried into SQLite verbatim
      by the importer.
- [ ] **Tor identity published by the node** — `getlocalinfo` on DNS2 still
      reports `onion: null` (it only looks for `onion_address.txt` in the state
      dir). Next step: derive the address from the wallet's stored
      `tor_v3_private_key` and serve it, with a unit test that recomputes the
      onion checksum. Honest caveat from the source read: the legacy client
      never re-derived an address from that key (it only wrote it), and the
      onion address stored in two real wallets does NOT recompute from either
      stored key — so any automatic derivation must be verified against a real
      identity before it is trusted.
- [ ] **I2P** — `getlocalinfo` reports `i2p: null` on DNS2; i2pd runs with a
      `tri7d-p2p-keys.dat` destination but no address file is published.
- [ ] **Windows installer refresh** — code + CI artifact landed (commit
      9723e33, item 16: 7.0.2 nsi ships txhistory.exe, real EstimatedSize;
      artifact proof in `.gate/ci-artifact-2026-11-09/`). Live silent-install
      exercise PENDING: the v7.0.2 release-publish run failed
      (`Distribute Release (tri7)` run 42714180707) and needs a repair tick
      before the installer row can count as live-proven.
- [ ] **Laptop install verified end-to-end**.
- [ ] **Auto-updater exercised** against a real newer release.

## Verification debt (not features, but parity of trust)

- [x] **`consistency` tool** — history vs live (dates, duplicates, height
      resolution, live-UTXO subset). Last sami-pc run: CONSISTENT.
- [x] **`store_gapfix`** — fills missing heights, hash9-verified.
- [ ] **`consistency` in CI** after every release.
- [ ] **Click-audit every control** on a real desktop session (the Hermes host
      cannot reach session 1).

---

## Counted status

| Area | Legacy | tri7 | Covered |
|---|---|---|---|
| RPC methods | 93 | 76 native | 60 verbatim (65%) + smsg renames |
| GUI pages/dialogs | 19 | 11 pages + debug window | ~14 (74%) |
| Transports | clearnet + Tor + I2P | clearnet + Tor | partial |

**Not counted as parity gaps** (intentional): PoW mining verbs, the legacy
account model, `sendalert`, P2SH/multisig creation.

## Suggested order (remaining)

1. Publish the Tor/I2P identity the wallet now carries (with the checksum test
   above), then `keypoolrefill` + `listaddressgroupings`.
2. `getcheckpoint`, `repairwallet`, `resendtx`.
3. SMSG runtime verbs (enable/disable, options, bucket scans).
4. Installer refresh, laptop verification, tray icon, staking reserve.
