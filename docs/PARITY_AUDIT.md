# PARITY AUDIT — triangles_v5 → tri7

**Audit scope:** every module in `triangles_v5` (SamiAhmed7777 fork, head `0484988`) vs `tri7` (head at audit time `427c3167`), per the parity-audit task. The "already carried over" baseline (consensus/replay/spend/stake/kernel, wallet read/write, smsg core+wire+GUI, txpool+broadcast, from-zero+gap recovery, difficulty, explorer index, GUI pages/palette/assets, auto-updater, installer, tridock/tri-pi, package managers) is treated as CARRIED and **not re-reported as missing**.

**Legend:** CARRIED = tri7 has it (file cited) · N/A = obsolete in tri7's architecture (one-line why) · GAP = useful and missing (recommendation + effort) · **[IMPL]** = implemented this audit.

---

## 1. Legacy src/ module inventory (every file)

| Legacy module | What it does | Status | tri7 note / recommendation |
|---|---|---|---|
| addressindex.h (+ -addressindex flag) | LevelDB per-address balance/UTXO/txid index → getaddressbalance/getaddressutxos/getaddresstxids | GAP | tri7's GUI explorer computes address views on demand from its O(1) disk index; the RPC family is the missing surface. **implement-lite** (~150 LOC over blockindex map; needs sibling blockindex.rs stable). Effort S–M |
| addrman.cpp/h | Peer address manager (tried/new tables, eviction) | CARRIED-lite | tri7 p2p.rs tracks peers with transport flags; simplified rotation is a deliberate cutover design (skill: multi-target attach). No action |
| allocators.h | Secure erase / arena allocators | N/A | Rust ownership + std; zeroize-style needs are local |
| base58.h | base58check codec | CARRIED | tri7 src/address.rs (daemon-verified vectors) |
| bignum.h | CBigNum MPI wrapper | N/A | primitive-types in tri7 |
| bip39_english.h + hdwallet.cpp/h | BIP39 mnemonic (coin type 2222) → BIP32 derive → key (hdnew/hdrestore/hdshow/hdinfo RPC, hdseeddialog GUI) | GAP | tri7 wallet.rs already parses/flags the `hdcmnemonic` record type but cannot CREATE HD keys. Useful for deterministic wallet recovery flows. **implement** (M, ~400 LOC: mnemonic+CKDpriv; BIP39 wordlist embed). Effort M |
| bootstrap.cpp/h + bootstrap_roots.h | HTTPS UTXO-snapshot bootstrap from pinned roots at first run | N/A | superseded: tridock ships the hash-verified chain snapshot; from-zero P2P sync is carried |
| chaindb_migrate.cpp/h | BDB→RocksDB chaindb migration | N/A | tri7 has no leveldb/rocksdb chaindb |
| checkpointpublisher.cpp/h | Build/verify signed checkpoint JSON (gencheckpoints/publishcheckpoint RPC, -checkpointkey) | GAP | Publishing infrastructure for trusted checkpoints. tri7 is zero-trust by design (full validation), so only the *publish* side is useful for others. **skip for now** (report only); revisit if Sami wants checkpoint publication from tri7d |
| checkpoints.cpp/h | Hardcoded height→hash checkpoint table (IBD trust anchors) + getcheckpoint RPC | N/A* | tri7 validates every block from genesis — strictly stronger than trusted anchors; getcheckpoint would return a table tri7 doesn't keep. *If Sami wants the RPC for tooling parity it is trivial JSON — **skip** |
| checkqueue.h | Parallel signature-check batch queue | N/A | single-threaded validation is an explicit tri7 design bound (explorer pressure principle) |
| clientversion.h | Version constants | CARRIED | TRI7_RPC_VERSION/SUBVERSION/PROTOCOL_VERSION in rpc.rs |
| coincontrol.h | Coin-selection control struct for GUI | CARRIED | tri7 GUI coin-control (skill: maturity filter pinned on both paths) |
| compat.h | sockets portability shims | N/A | std::net |
| crypter.cpp/h (+ pbkdf2.cpp/h, scrypt.cpp/h) | Wallet encryption: scrypt-KDF master key, AES-256-CBC ckey/mkey records, walletpassphrase family | GAP | The GUI still shows "Encrypt Wallet…" (stub action) and tri7 refuses encrypted wallets. Real demand (Sami's bar: no loss of functionality). **implement** (L, ~600+ LOC: scrypt KDF, mkey/ckey read/write, passphrase RPCs + GUI). Effort L — report only this round |
| crypto_ecdh.cpp/h | ECDH for smsg | CARRIED | src/smsg.rs (byte-exact port) |
| crypto_ecdsa.cpp/h | ECDSA sign/verify + recovery | CARRIED | spend.rs / wallet.rs / smsg.rs (secp256k1 crate) |
| db.cpp/h (walletdb-bdb base) | BerkeleyDB wallet backend | N/A | tri7 is SQLite-native; BDB import-only (sami-pc artifacts) |
| feeestimator.cpp/h | estimatefee smart estimator | CARRIED-as-constant | legacy estimatefee ignores its estimator and returns static MIN_TX_FEE; **[IMPL]** estimatefee returns that exact policy (100 sats) in tri7 |
| hashblock.h | Hash9 (13 sphlib rounds) | CARRIED | legacy.rs hash9 (FFI sphlib) |
| i2p.cpp/h, i2p_process.cpp/h | Embedded I2P client + process mgmt | N/A | standing rule: system i2pd via SOCKS 4447 — no bundled transports. tri7 dials .b32.i2p (carried) |
| init.cpp | Daemon bootstrap, flag table (~70 flags) | CARRIED | tri7d (follow_tip.rs) — tri7-native flag set; flag-for-flag parity not a goal (see §4) |
| kernel.cpp/h | PoS kernel/stake modifier | CARRIED | kernel.rs (byte-exact, live-verified) |
| key.cpp/h | secp256k1 key ops | CARRIED | wallet.rs/spend.rs |
| keystore.cpp/h | Key store base | CARRIED | wallet.rs read/write |
| main.cpp/h | Relay/accept/mempool/orphan/fork-alert (nForkAlertCount), SSE hooks | CARRIED | p2p.rs + follow_tip.rs acceptance path; SSE → GAP below; FORK ALERT log → superseded by deploy/fleet_tip_monitor.py (carried) |
| miner.cpp/h | PoW/PoS block creation loop | CARRIED | stake.rs + stake_mint + integrated staking loop |
| mruset.h | Bounded recent-set container | N/A | std collections |
| net.cpp/h | Peer I/O, transports, seeds, upnp/irc | CARRIED | p2p.rs (clearnet+tor+i2p transports, inbound listener+relay); upnp/irc obsolete |
| net_bootstrap.cpp/h | NetworkHealth aggregation | N/A | getnetworkstability (§2) covers the useful slice — GAP-lite below |
| netbase.cpp/h | SOCKS/proxy resolution | CARRIED | system tor/i2p SOCKS dialing in p2p path |
| noui.cpp | Non-GUI UI stubs | N/A | logging in tri7d |
| onionseed.h | Hardcoded onion seed list + dynamic seeds.txt fetch | CARRIED-lite | tri7d default rotation targets carry the fleet seeds; the dynamic HTTPS seeds.txt fetch is **GAP-lite: skip** (config-file seeds cover it) |
| openssl_compat.h | OpenSSL 1.1 shims | N/A | no OpenSSL in tri7 |
| protocol.cpp/h | Wire protocol types | CARRIED | p2p.rs/legacy.rs |
| regen_genesis.cpp | Dev utility to regenerate genesis | N/A | one-off dev tool |
| rest.cpp/h | HTTP GET REST API (/rest/chaininfo|block|blockheader|tx|blockhashbyheight|blockbyheight|mempool|difficulty|wallet) with .json/.hex forms | GAP | Explorer/tools convenience over plain GET. **implement-lite** (~150–200 LOC in rpc.rs serve(): route GET /rest/<res>[.json|.hex] onto the existing native handlers). Effort S–M — report only this round |
| rpc_httpsocket.h | RPC over unix socket transport | GAP-lite | niche (Sami uses loopback TCP). **skip** |
| rpcblockchain.cpp | getblock/getblockhash/getblockheader/getchaintips/invalidateblock/reconsiderblock/recalculatesupply/dumputxoset/gettxoutsetinfo/estimatefee/auditsignatures | mostly CARRIED / GAP | see §2 per method |
| rpcdump.cpp | dumpprivkey/importprivkey/dumpwallet/importwallet | GAP → **[IMPL]** | key export/import shipped this round as lib keytool + wallet_keytool bin (legacy dumpwallet format byte-compatible; WIF version 193). RPC forms need ops wiring (report: ~30 LOC each in follow_tip ops dispatch once the sibling task lands) |
| rpcmining.cpp | getsubsidy/getmininginfo/getstakinginfo/reservebalance | CARRIED / **[IMPL]** | getstakinginfo was carried (ops); getsubsidy/getmininginfo **[IMPL]**; reservebalance → GAP below |
| rpcnet.cpp | getconnectioncount/getpeerinfo/addnode/disconnectnode/getseedlist/getnetworkstability | CARRIED / GAP | getconnectioncount/getpeerinfo carried; addnode/disconnectnode → GAP (ops wiring); getseedlist/getnetworkstability → GAP-lite |
| rpcsmessage.cpp | smsg_* RPC family (14 methods) | mostly CARRIED | tri7 carries smsgsend/smsginbox/smsgoutbox/smsgscandir(≈scanbuckets)/smsgpurge/smsgaddaddress(≈addkey)/smsgkeys(≈localkeys)/smsglocalinfo; anon send supported in engine (smsgsendanon shape); see §2 for the stragglers |
| rpcwallet.cpp | wallet RPC family (send/getnew/list/accounts/sign…) | mixed | see §2 per method |
| rpcrawtransaction.cpp | raw tx decode/build/sign/send | mixed | sendrawtransaction carried; decode/build/sign → GAP (see §2) |
| scan_chain_tip.cpp | Standalone blk-file tip scanner utility | N/A | validate_chain bin supersedes |
| serialize.h | Wire codec | CARRIED | legacy.rs |
| sigcache.h | Signature verification cache | N/A | performance-only; validation load is modest by design |
| smessage.cpp/h | smsg engine | CARRIED | smsg.rs + smsg_net.rs (byte-exact; live roundtrip both directions) |
| snapshotnet.cpp/h | P2P UTXO-snapshot distribution (custom messages, chunk striping) | N/A | superseded architecture (tridock snapshot + from-zero block sync). Worth revisiting ONLY if from-zero sync ever proves too slow in practice |
| sph_*.h (13) | sphlib hash primitives | CARRIED | vendor/ FFI |
| strlcpy.h | safe strcpy | N/A | Rust strings |
| sync.cpp/h | Lock-order profiler | N/A | dev tool; Rust mutex discipline |
| syncmanager.cpp/h | Header-download window/parallel IBD pipeline (v6.1) | N/A | superseded by tri7 windowed follow + gap recovery + planned parallel fetchers (from-zero notes) |
| tor_embed_hooks.cpp/h | Embedded tor control hooks | N/A | standing rule: system tor via SOCKS (no bundled daemons) |
| triangles-cli.cpp | CLI JSON-RPC client binary | GAP-lite | curl covers it; a tiny `tri7-cli` (protocol-compatible print formatting) is **skip/skip-lite** |
| trianglesrpc.cpp/h | JSON-RPC server (table, HTTP, batching) | CARRIED | rpc.rs (native-first, batch arrays, envelope parity) |
| txdb-*.cpp/h (base/factory/leveldb/rocksdb) | Chain DB backends | N/A | tri7 disk index + window (task says N/A explicitly) |
| ui_interface.h / notificationqueue.h | Qt UI signaling | N/A | egui app state |
| uint256.h | 256-bit ints | CARRIED | primitive-types Hash |
| util.cpp/h | Global utils, debug log, fee globals, warnings | CARRIED | tri7d logging + rpc errors; no hidden gaps worth porting |
| util_signal.h | Graceful signal shutdown | CARRIED | tri7d --forever/systemd lifecycle (follow_tip) |
| utxosnapshot.cpp/h | Load/serve UTXO snapshots (assumevalid-style fast start) | N/A | superseded (full replay + disk index; "no trust shortcuts" is a tri7 standing rule) |
| version.cpp/h | Version constants | CARRIED | client-version constants |
| zmqpublishnotifier.cpp/h | ZMQ pub hashblock/hashtx (-zmqpub flags) | GAP | Event push for external monitors. tri7's consumers poll RPC/health today. **skip** (recommend revisit if a fleet consumer needs push; effort M + new dep) |
| wallet.cpp/h | Wallet core: keys, accounts, tx bookkeeping, send paths | CARRIED / GAP | keys+records carried; accounts subsystem → skip (obsolete UX); sendtoaddress/sendmany/sendfrom RPC → GAP (ops wiring, M); listtransactions/gettransaction/listsinceblock → GAP (M; needs wallet-tx reconstruction over the index) |
| walletdb-*.cpp/h (bdb/sqlite/factory/batch/recover) | Wallet DB backends | CARRIED / N/A | SQLite backend carried (wallet.rs, byte-verified); bdb/factory/recover N/A (salvage → covered by checkwallet **[IMPL]** in keytool) |
| walletmigrate.cpp/h | BDB→SQLite wallet migration | N/A | import wallet.dat keys via wallet_keytool dump→import round trip (this round) covers the need going forward |

## 2. Legacy RPC method table — every method (106)

C = tri7 native · P = proxy fallback (transitional) · **[IMPL]** = added this audit · GAP = missing

| Method | Status | Note / recommendation |
|---|---|---|
| help | **[IMPL]** | native+fallback method list; per-command membership |
| stop | GAP | needs ops shutdown path; `systemctl restart tri7d` is the ops answer — **skip** (report) |
| getbestblockhash | C | rpc.rs native |
| getblockcount | C | |
| getconnectioncount | C | |
| getpeerinfo | C | ops engine, transport flags |
| addnode | GAP | peer dial is in follow_tip ops — **implement** (~40 LOC ops arm once sibling lands). Effort S |
| disconnectnode | GAP | same — **implement** with addnode. Effort S |
| getdifficulty | C | daemon-parity shape |
| getblockheader | **[IMPL]** | verbose subset of the frame block JSON; raw form = 80-byte header hex; legacy index-only fields (mint/blocktrust/chaintrust) null |
| getblockchaininfo | **[IMPL]** | tip+difficulty+supply w/ coverage fields |
| getwalletinfo | **[IMPL]** | snapshot balance/coverage; keypool/txcount honestly null |
| getnetworkinfo | **[IMPL]** | version/subversion/protocol/connections/uptime/transports |
| getseedlist | GAP | seed list lives in follow_tip config — **implement-lite** (~20 LOC ops arm) later. Effort S |
| getnetworkstability | GAP-lite | peer-quality aggregate from peer table — **implement-lite** (~40 LOC) later. Effort S |
| gettxoutsetinfo | GAP | chain-wide UTXO count not in tri7's wallet-scoped snapshots; supply+height available. **implement-lite** when blockindex exposes chain UTXO counts. Effort M |
| estimatefee | **[IMPL]** | exact legacy static policy (MIN_TX_FEE = 0.0001 TRI) |
| getaddressbalance | GAP | addressindex family (see §1) |
| getaddressutxos | GAP | ditto |
| getaddresstxids | GAP | ditto (heaviest of the three) |
| getinfo | C | |
| getsubsidy | **[IMPL]** | legacy PoW schedule (0 above h9001) |
| getmininginfo | **[IMPL]** | composes tip+difficulty+ops staking stats |
| getstakinginfo | C | ops engine |
| getnewaddress | GAP | wallet WRITE via RPC — deliberately not proxy-forwarded (write-safety). **implement** as an ops method after the sibling wallet task lands (add_key exists in wallet.rs). Effort S+M |
| getnewpubkey | GAP | with getnewaddress. Effort S |
| getaccountaddress / setaccount / getaccount / getaddressesbyaccount / listaccounts / move / sendfrom | GAP (skip) | legacy "accounts" subsystem (bitcoin-0.8 UX); recommend **skip** — obsolete model, labels cover the need |
| sendtoaddress | GAP | **implement** (ops arm: select coins → spend.rs build/sign → sendrawtransaction; ~120–180 LOC once sibling lands). Effort M |
| sendmany | GAP | same path, multi-out. Effort M |
| getreceivedbyaddress | C | snapshot received map |
| getreceivedbyaccount / listreceivedbyaccount | GAP (skip) | accounts subsystem |
| listreceivedbyaddress | GAP-lite | trivially derived from balances snapshot; **implement-lite** later. Effort S |
| backupwallet | C | tri7 auto .tri7bak on first write (wallet.rs); a manual `backupwallet` ops arm is trivial later |
| keypoolrefill | GAP (skip) | tri7 keeps no keypool |
| walletpassphrase / walletpassphrasechange / walletlock / encryptwallet | GAP | wallet-encryption feature (§1 crypter); **implement** as one feature. Effort L |
| validateaddress | C | |
| validatepubkey | GAP-lite | pubkey→address shape check, trivial; **implement-lite** (~15 LOC) later. Effort S |
| getbalance | C | |
| getrawmempool | GAP | txpool lives behind ops; **implement** (~15 LOC ops arm reading TxPool::json()) once sibling lands. Effort S |
| getmempoolinfo | GAP | ditto |
| getblock | C | verbose+raw forms |
| getblockbynumber | **[IMPL]** | height alias of getblock |
| getblockhash | C | |
| gettransaction | P | proxy fallback (wallet-tx detail native form = GAP, see listtransactions) |
| listtransactions | GAP | needs wallet-tx history reconstruction over the index. Effort M–L |
| listaddressgroupings | GAP-lite | from balances snapshot grouping; **implement-lite**. Effort S |
| signmessage | GAP | GUI has it (gui::state sign_message); RPC needs a key via ops — **implement** (~25 LOC ops arm). Effort S |
| verifymessage | **[IMPL]** | compact-recoverable parity via the shared gui::state implementation |
| listsinceblock | GAP | like listtransactions. Effort M |
| dumpprivkey | GAP (RPC form) | **[IMPL as bin]** wallet_keytool dumpprivkey; RPC arm = ~15 LOC when ops lands |
| hdnew / hdrestore / hdshow / hdinfo | GAP | HD wallet feature (§1). Effort M (with hdwallet port) |
| dumpwallet / importwallet | GAP (RPC form) | **[IMPL as bin]** wallet_keytool dumpwallet/importwallet (legacy format); RPC arm = ~30 LOC when ops lands |
| importprivkey | GAP (RPC form) | same — bin importwallet covers the capability |
| listunspent | C | coverage-gated |
| getrawtransaction | C | verbose+hex |
| createrawtransaction | GAP | spend.rs Solver builders exist; **implement** (~60–100 LOC over legacy.rs Tx). Effort S+M |
| decoderawtransaction | GAP | format_tx_json exists — **implement** (~40 LOC). Effort S |
| decodescript | GAP-lite | script→asm/address shape; **implement-lite**. Effort S |
| signrawtransaction | GAP | spend.rs SignatureHash port exists; an RPC wrapper needs selected coins via ops. Effort M |
| sendrawtransaction | C | ops broadcast (live-verified) |
| getcheckpoint | GAP (skip) | no checkpoint table in tri7 (by design) |
| gencheckpoints / publishcheckpoint | GAP (skip) | publisher infra (§1 checkpointpublisher) |
| getchaintips | GAP | fork-branch data beyond tri7's single live window; **skip** (report; would need window branch bookkeeping) |
| invalidateblock / reconsiderblock | GAP (skip) | debug tools; tri7's rollback path exists internally (gap recovery) but is not RPC-safe — report only |
| recalculatesupply | GAP (skip) | balances bin recomputes supply offline (carried) |
| auditsignatures | C-as-bin | bin/verify_sigs.rs + historical_sig_corpus.rs are the tri7 equivalent |
| dumputxoset | GAP (skip) | balances bin --utxos-out is the equivalent artifact |
| reservebalance | GAP | staking reserve (keep N TRI unspent); useful with live staking on all wallets. **implement** (~40 LOC in stake candidate filter + conf knob) after sibling stake files settle. Effort S+M |
| checkwallet / repairwallet | **[IMPL]** / skip | checkwallet **[IMPL]** in wallet_keytool (integrity_check + key re-derivation); repairwallet N/A (BDB-era orphan repair doesn't exist in SQLite format) |
| resendtx | GAP (skip) | txpool rebroadcast — revisit with mempool ops arm |
| abandontransaction | GAP (skip) | wallet-tx state machine tri7 doesn't track yet |
| makekeypair | GAP-lite | generate+return a keypair (no wallet write); **implement-lite** (~20 LOC). Effort S |
| smsgenable / smsgdisable | GAP-lite | tri7 toggles via conf/stake-style persisted state; **implement-lite** (ops flag). Effort S |
| smsglocalkeys | C (as smsgkeys/smsglocalinfo) | renamed surface, same capability |
| smsgoptions | GAP-lite | legacy get/set of ~5 options; **implement-lite** over the options file. Effort S |
| smsgscanchain | N/A | chain-scan for historical smsg — tri7's smsgscandir covers the store side; chain-rescan needs full smsg history in the index (**skip** unless Sami asks) |
| smsgscanbuckets | C (as smsgscandir) | |
| smsgaddkey | C (as smsgaddaddress) | |
| smsggetpubkey | GAP | pubkey lookup for an address — exists internally (store.lookup_pubkey); **implement** (~20 LOC ops arm). Effort S |
| smsgsend / smsginbox / smsgoutbox | C | ops engine (live roundtrip verified) |
| smsgsendanon | C-equivalent | engine supports anon (from=None); expose via smsgsend arg if the GUI needs it |
| smsgbuckets | GAP-lite | tri7's smsgscandir/purge cover storage queries; drop-count summary is trivial to add. Effort S |
| smsgbroadcast | GAP-lite | raw-packet broadcast; rarely used by humans — **skip** unless needed |

**Gap count:** 44 missing legacy surfaces total (including skip-classified), of which **10 RPC methods implemented this round**, 2 tools shipped (wallet_keytool dump/import/check/dumpprivkey), and 6 small ops-wiring items queued behind the sibling in-flight tasks.

## 3. Legacy qt forms (src/qt/forms, 19 files)

| Form | Status | tri7 GUI note |
|---|---|---|
| mainwindow.ui | CARRIED | egui shell, sidebar, header branding |
| overviewpage.ui | CARRIED | Overview page |
| sendcoinsdialog.ui / sendcoinsentry.ui | CARRIED | Send page + recipients |
| transactionspage.ui | CARRIED | Transactions page (filters) |
| transactiondescdialog.ui | CARRIED | tx detail view (explorer tx view) |
| addressbookpage.ui | CARRIED | Address book page |
| editaddressdialog.ui | CARRIED | add/edit contact |
| optionsdialog.ui | CARRIED | Options page |
| rpcconsole.ui | CARRIED | Debug console (RPC) |
| aboutdialog.ui | CARRIED | About (CARGO_PKG_VERSION) |
| qrcodedialog.ui | CARRIED | receive-page inline QR + save |
| coincontroldialog.ui (+ coincontroltreewidget) | CARRIED | coin control (maturity parity pinned) |
| askpassphrasedialog.ui | GAP | with wallet encryption (crypter) — the GUI button is a stub today |
| messagepage.ui / sendmessagesdialog.ui / sendmessagesentry.ui | CARRIED | smsg Messages page + compose |
| signmessagepage.ui / verifymessagepage.ui | CARRIED | sign/verify pages (shared state fns now also serve the RPC) |
| hdseeddialog.ui | GAP | HD wallet feature (§1) |
| introdialog.ui | CARRIED | load screen with wallet/state pickers (rfd native dialogs) |

Support files: csvmodelwriter (CSV export of address book/transactions) → **GAP-lite: implement** (~40 LOC, save-dialog + CSV write) — nice-to-have, not blocking. notificator (desktop notifications) → **skip** (headless hosts; GUI toast later). qtipcserver / macdock / monitoreddatamapper / outlinedlabel / dialog_move_handler / guiutil → N/A (Qt plumbing; egui equivalents carried where visible).

## 4. init.cpp / net.cpp / main.cpp feature flags

| Legacy flag/feature | Status |
|---|---|
| -tor/-notor/-torsocks/-tordatadir/-torconnecttimeout | CARRIED (system tor via SOCKS; flags differ by design) |
| -i2p (own i2p client) / i2p_process | N/A (system i2pd via SOCKS 4447 — standing rule) |
| -proxy/-socks/-onlynet/-externalip/-listen/-bind/-connect/-maxoutboundconnections/-discover/-dns/-seednode | CARRIED-equivalent (tri7d config/rotation covers the operational subset) |
| -daemon/-server/-printtoconsole/-debug/-logtimestamps/-shrinkdebugfile | CARRIED-equivalent (tri7d logging/service) |
| -testnet | N/A (single mainnet target; testnet seeds empty in legacy too) |
| -reindex/-rescan/-loadblock/-fastindex/-rebuildutxo/-autorerebuild/-zapwallettxes/-salvagewallet/-upgradewallet/-detachdb | N/A / superseded (blk-file scan + balances bin + keytool check; wallet upgrades don't exist yet) |
| -bootstrap/-nobootstrap/-snapshot/-snapshottimeout/-acceptanylocalsnapshot | N/A (tridock snapshot + from-zero) |
| -addressindex/-addrlifespan | GAP (addressindex family) |
| -checkpointkey | GAP (skip — checkpoint publisher) |
| -paytxfee/-mininput/-settxfee/RPC settxfee | GAP-lite: **skip** (static fee policy; revisit if fees ever matter) |
| -reservebalance | GAP (see reservebalance RPC — implement after siblings) |
| -minersleep/-par/-confchange/-enforcecanonical/-cppolicy | N/A (staking loop constants; policy flags vestigial) |
| -sseNotify (SSE /events on RPC port: new block/tx push) | GAP: modern equivalent of ZMQ push; **skip** for now, revisit with ZMQ decision |
| -zmqpub/-zmqpubhashblock/-zmqpubhashtx | GAP: skip (see zmqpublishnotifier §1) |
| -onionseed (dynamic seeds.txt fetch) | GAP-lite: skip (config seeds) |
| -migratechaindb/-migratewallet | N/A (no legacy backends in tri7) |
| FORK ALERT (lag vs peer-median stderr alert, main.cpp:93-118) | CARRIED-equivalent (deploy/fleet_tip_monitor.py with lag-vs-fork discrimination — richer than the legacy log line) |
| ORPHAN pool processing | CARRIED (txpool/p2p acceptance path) |
| SSE block/tx notifications | GAP (same as -sseNotify above) |

## 5. wurstgelee-lineage extras (v6-era additions, 2026 copyright files)

bootstrap.cpp, checkpointpublisher.cpp, chaindb_migrate.cpp, crypto_ecdh/ecdsa, hdwallet.cpp, snapshotnet.cpp, syncmanager.cpp, utxosnapshot.cpp, walletdb-sqlite/factory/recover, walletmigrate.cpp, zmqpublishnotifier.cpp, addressindex.h, bootstrap_roots.h, notificationqueue.h, rpc_httpsocket.h, util_signal.h — each classified above. Net result: **the v6 extras tri7 genuinely lacks are HD wallet, wallet encryption, the address-index RPC family, and the event-push channels (ZMQ/SSE); everything else is either carried or superseded by a stronger tri7 mechanism.**

## 6. Implemented this round (files, tests, verification)

- `src/rpc.rs` — 10 new native methods: `help`, `getblockheader` (verbose+raw 80-byte-header hex), `getblockbynumber`, `getblockchaininfo`, `getwalletinfo`, `getnetworkinfo`, `getmininginfo` (composes ops staking stats when live, nulls when down), `getsubsidy` (legacy PoW schedule), `estimatefee` (exact static policy), `verifymessage` (compact-recoverable parity via the shared sign/verify implementation). Legacy-index-only fields (mint/blocktrust/chaintrust, keypool) are emitted null — never invented.
- `src/keytool.rs` (new lib module) — WIF codec at the chain's secret version byte **193** (128 + PUBKEY_ADDRESS 65, base58.h:410 — NOT bitcoin's 0x80), legacy dumpwallet format (`WIF epoch label=… # addr=…`, `change=1`/`reserve=1`, `\xNN` escapes), import parser matching rpcdump.cpp grammar, dump→import round trip through the real wallet read/write paths (open_readwrite rails: auto-backup, refuses encrypted), dumpprivkey, checkwallet (PRAGMA integrity_check + per-key secret→address re-derivation). Import is compressed-form-only BY DESIGN: an uncompressed-form WIF maps the secret to a different address (hash160 of the 65-byte pubkey) and tri7's write path stores compressed rows — import refuses it loudly instead of silently binding the wrong address (export keeps the flag faithful).
- `src/bin/wallet_keytool.rs` (new bin) — CLI: `dumpprivkey`, `dumpwallet [tip_height tip_hash]`, `importwallet`, `checkwallet`; JSON report on stdout, distinct exit codes. End-to-end proof on a synthetic wallet: import (auto .tri7bak backup) → checkwallet ok → dump (header+best-block line) → dumpprivkey round-trip → idempotent re-import (0 imported / 2 skipped / 0 failed).
- 13 new tests (keytool: 8 incl. an independently-derived python WIF vector, roundtrip through the real write path, idempotent import, uncompressed refusal, encrypted-container refusal with a real mkey-bearing fixture; rpc: 8 incl. subsidy schedule, header-vs-block shape, raw header slice, static fee, verifymessage true/false/-5 paths, ops-composition nulls).

**Verification (live repo, audit-time tree):** `cargo test --release --lib` → **278 passed / 0 failed / 1 ignored** (the ignored test is the deliberate live-wallet-evidence gate), EXIT=0. Full multi-target `cargo test --release` additionally passes (EXIT=0) with one 3-line test-fixture stub in a sandbox copy — the live tree's `bin follow_tip` test fixture (`follow_tip.rs:6683`, missing the new WalletKeys HD fields) is owned by the concurrent sibling task and was left untouched per the task's file ban; every other target is green either way.

### Note on concurrent sibling work
While this audit ran, a sibling task landed (uncommitted at audit time) `src/seed.rs` (BIP39/BIP32), `src/gui/seed_flow.rs` (HD create/restore GUI flow), and HD record support in `wallet.rs` — i.e. the **HD-wallet GAP in §1/§2 (hdnew/hdrestore/hdshow/hdinfo, bip39_english.h + hdwallet.cpp)** is being closed by that task in the same window. This audit's classification stands for committed head `427c3167`; re-check HD status at merge time.

### Known follow-ups queued behind sibling in-flight files
`addnode`, `disconnectnode`, `getrawmempool`, `getmempoolinfo`, `getseedlist`, `signmessage`, `dumpprivkey`/`dumpwallet`/`importwallet` RPC forms, `getnewaddress`, `sendtoaddress`, `sendmany`, `reservebalance`, `smsggetpubkey` — each needs a small ops-dispatch arm in `follow_tip.rs` (sibling-owned) or the live wallet handle; all have their data sources ready in the lib (txpool.rs `json()`, spend.rs builders, wallet.rs `add_key`, keytool exports).
