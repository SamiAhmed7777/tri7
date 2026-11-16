# Stage-2 execution plan — balance recovery + real on-chain spend (sami-pc, against a COPY)

Kind: deploy (live-funds operation on the operator's PC; SPENDS REAL COINS)

## Ground truth measured (2026-11-15, DNS2 addrindex complete at h2212436+)
- Canonical operator wallet: E:\Coins\TRI\wallet.dat (BDB, 37,511,168 bytes, mtime Nov 10, launched by triangles-qt -datadir=E:\Coins\TRI). 399 keys, 188 labels, not encrypted. tri7 read_wallet parses it.
- **14.53853420 TRI key-held** across 9 funded addresses in that key set (8.44 + 3.07 + 3.02 + dust incl. TX8YeXumu… = 0.00379073 TRI).
- 2.63 TRI on two watch-only addresses (TKFyBB99…, TH3W6sTw…): keys in NO copy examined (Pi's 103-key, DNS2's 248-key, canonical 399-key, E:\ dump 123 addrs) — pre-zap backup archaeology is a SEPARATE decision for Sami; NOT in this plan.
- Standing directive (tri7-fleet-chain-state/references/pi-full-node-and-wallet-home.md): the Pi never touches funded wallets; these ops run on sami-pc against a COPY of the canonical wallet. This plan complies.

## What the proof IS
Per the Stage-2 definition: (a) the legacy wallet's funds recoverable into ONE tri7 wallet — for the canonical wallet this is the BDB→SQLite migration itself (bdb_migrate; its keys are already complete — an importwallet stage would be the documented no-op); (b) a REAL on-chain spend from migrated funds: a self-spend of the TX8YeXumu coin with 100-sat fee, destination = a fresh unused address of the SAME wallet (T1KUdGhu64ELGjuoHScuN1Hdj3VQcZRrmzFCrkK8o) — nothing leaves the operator's ownership (the reference's own spend-fee precedent: 100 sats on a 1 TRI output passed).

## Steps (all on sami-pc except DNS2-side relay/verify)
1. Ship the fresh Windows tool bins (built clean from master 1075100 on DNS2, 1m49s: wallet_keytool.exe, bdb_migrate.exe, read_wallet.exe, spend_pinned.exe) → C:\Users\hello\tri7\recovery\
2. COPY E:\Coins\TRI\wallet.dat → E:\Coins\TRI\recovery-work.dat (the original is never opened for write; triangles-qt is NOT running — verify process absent first).
3. bdb_migrate recovery-work.dat (non-destructive: verbatim record copy → temp SQLite, row-count verify, .bak of the input, swap). Expect: row-count verify PASS.
4. read_wallet recovery-work.dat → must report 399 keys, encrypted: false (the migration's own assertion).
5. RPC reachability for the parent-tx fetch: SSH local port forward sami-pc → DNS2 (100.121.150.22) tunneling 127.0.0.1:19112. sami-pc's ssh client tested first; if sami-pc cannot ssh out, STOP and report (fallback requires a DNS2-side portproxy decision = new listening surface on a live node — needs Sami's call).
6. spend_pinned (wallet opened READ-ONLY; nothing broadcast by it):
   --wallet E:\Coins\TRI\recovery-work.dat --rpc-port 19112
   --prev-txid 353be99221075f96d9e92e745da8f50b2740add1328cc520cefd55f438a76005
   --prev-vout 1 --prev-height 2200281 --prev-time 1775454304
   --dest T1KUdGhu64ELGjuoHScuN1Hdj3VQcZRrmzFCrkK8o
   --amount 378973 --fee 100 --out E:\Coins\TRI\recovery-spend.json
   (379,073 − 100 = 378,973 sats; exit 0 = signed + self-verified)
7. scp recovery-spend.json → DNS2; sendrawtransaction(tx_hex) via DNS2's RPC; wait for the next minted/adopted block; verify getaddressbalance: dest +378,973, source drained; three-way tip parity unaffected.
8. Keep E:\Coins\TRI\recovery-work.dat (the recovered one-wallet artifact); report paths + hashes. Cleanup: kill the ssh tunnel; no other residue.

## Rollback
- The original E:\Coins\TRI\wallet.dat is never written. bdb_migrate's .bak + the copy pattern make step 3-4 reversible by deletion. A rejected tx (step 7) = nothing happened on-chain; the coin stays.

## Risk assessment
- Real coins moved: 0.00379073 TRI self-transfer to the operator's own fresh address. Loss risk ≈ fee (100 sats) if the tx reorgs away (chain is stable, 3-way parity).
- The key material stays in files on sami-pc; the tunnel carries only RPC JSON.
- triangles-qt must not run during step 2-4 (file-copy consistency); verified before proceeding.
