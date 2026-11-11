# Seed-phrase wallet tooling + multi-input send self-reject fix

## 1. NEW: examples/wallet_from_seed.rs
Creates a tri7 wallet file from a BIP39 24-word phrase using the wallet's own
HD path (seed::derive_triangles, m/44'/2222'/0'/0/i, empty passphrase — the
same convention sweep.rs uses). --generate prints the phrase ONCE at end of
stdout; --mnemonic rebuilds and must byte-match addresses (roundtrip verified
for both deployed wallets: Pi TWk5CPqb..., DNS3 TLqn1NwP...).

## 2. FIX: multi-input sends self-rejected ("prevout map misaligned with vin")
build_spend_outputs signs the tx with vin in (txid,n) SORTED order, then runs
its internal connect_inputs_check with the same sorted list (passes). But the
daemon's sendtoaddress (follow_tip ~5103) and the GUI send validation
(gui/state.rs ~1465) re-ran connect_inputs_check (and verify_input) with the
UNSORTED selection list — the zip misaligns on any multi-input spend and the
send self-rejects. Fix: sort a copy of the selection into vin order at both
call sites. Single-input spends were unaffected (tonight's 1-input dryrun
passed; every real multi-input sendtoaddress failed on both DNS2 and the PC).

## 3. Fleet ops performed with this code (evidence)
- store_gapfix (existing tool) repaired DNS2's store hole h2206928..h2206952
  (25/25 verified against tipstate authority via an SSH RPC tunnel), index
  cleared, node restarted: [blockidx] threaded_tip=Some(2206978), wallet live
  view ready (214 outputs), staking enabled.
- Two real sendtoaddress sends from DNS2 then SUCCEEDED (previously impossible):
  d7612285... 100 TRI -> Pi TWk5CPqb..., ca2ec8c4... 100 TRI -> DNS3 TLqn1NwP...
- Pi + DNS3 now run BIP39 seed wallets (fresh keypairs born on-node; funded
  with 100 TRI each per operator instruction "100 TRI each").
- Operator's phrases delivered as files to C:\Users\hello\Tri7Seeds\
  (pi-seed.txt 163 B, dns3-seed.txt 154 B) — never printed to chat.

## 4. Tests
cargo test --release --lib: 455 passed, 0 failed.
