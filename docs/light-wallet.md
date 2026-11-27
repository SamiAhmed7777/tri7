# tri7 light wallet v1 — watch-only monitor

`tri7_light_wallet` is a light, no-node, no-chain-download watcher for
explicitly listed TRI addresses. It polls the public TRI explorer's REST
surface and prints **explorer-reported** balances, UTXO counts and recent
history. It is the monitoring half of the Electrum-style agent wallet
(pipeline P6.2); the acting half (spending, staking) stays on a full tri7
node.

```
air-gapped box:  BIP39 24-word seed ──► m/44'/2222' ──► address list
                  (triangles-paper-wallet, never online)
light client:    address list ──► watch balances / UTXOs / history
                  (this tool — never sees a seed)
```

## Build

```
cargo build --release --features light-wallet --bin tri7_light_wallet
```

The `light-wallet` feature pulls in ureq/rustls (HTTPS); headless `tri7d`
builds stay TLS-free without it, and `cargo build` without the feature
skips this binary entirely.

## Usage

```
# one address
tri7_light_wallet --address TRsxnwXeTvbdvwjj61TPLxmePqWHkfaagH

# a watch list file (one TR… address per line, '#' comments allowed)
tri7_light_wallet --addresses agent-addrs.txt

# machine-readable output for agents, polling every 60 s
tri7_light_wallet --addresses agent-addrs.txt --json --watch 60

# options
--explorer URL   explorer REST base (default: the public TRI explorer)
--max-lag N      fail closed if the address index lags the chain tip by
                 more than N blocks (default 10)
--retries N      retries per address per refresh (default 2)
--recent N       recent txids shown per address (default 5)
--timeout N      per-read HTTP timeout in seconds (default 30)
```

Exit codes: `0` every address OK; `2` some address INCONSISTENT or
UNAVAILABLE (fail closed — never a guessed number); `1` usage/config or
refresh-level transport error.

Addresses are validated as mainnet P2PKH (version 65) before anything is
fetched. Address discovery is explicit in v1: the config names the exact
addresses; there is no gap scanning and no xpub input (deferred to a later
version with cross-tool CKDpub parity tests).

## Trust model — what the tool guarantees and what it does not

Every figure is **explorer-reported**, never "spendable-verified". The
explorer's `chain_tip` / `index_complete` / `index_height` fields are the
server's own assertions, not proofs; HTTPS protects transit only. The tool
fails closed instead of guessing:

- **Health gate** — every read must report `index_complete: true` with an
  index height within `--max-lag` blocks of the reported chain tip. (The
  live addrindex runs a few blocks behind the tip during normal operation
  while reporting complete; a hard equality gate would fail every real
  read, so v1 bounds the lag and discloses the observed heights instead.)
- **Snapshot coherence** — only the `/rest/address/<A>/balance` response
  carries the observation anchor (`chain_tip`, `index_height`,
  `index_complete`); the utxo/txid responses are bare arrays. One refresh
  anchors on the balance response and enforces `balance == Σ utxo
  satoshis` as the cross-call coherence check. A mismatch is retried, then
  reported as an INCONSISTENT view with the observed heights — never as a
  number.
- **Divergence rule** — an address whose explorer-reported `received`
  exceeds the whole chain money supply while the addrindex UTXO view is
  empty (the known unresolved addrindex-vs-scanner accounting divergence
  seen on high-turnover addresses) is reported INCONSISTENT/UNAVAILABLE —
  never a bare 0 and never any second-scanner figure.
- **Transport hardening** — HTTPS with certificate validation (plain HTTP
  is only allowed for loopback hosts, used by the test stubs), redirects
  are never followed, connect and per-read timeouts, a 10 MiB response
  cap, `application/json` content type required, and strict typed JSON —
  anything malformed fails closed.
- **Strict JSON schema** — responses are parsed into typed structs with
  `deny_unknown_fields`, pinned to the live explorer REST surface
  (2026-11-26): `chaininfo` = chain/blocks/bestblockhash/difficulty/
  moneysupply; `balance` = balance/chain_tip/index_complete/index_height/
  received; utxo entries = address/height/outputIndex/satoshis/script/
  txid. If the explorer ever adds or drops a field, this tool fails closed
  until its schema is updated — deliberate, so a schema drift can never
  silently degrade a read.
- **URL hardening** — the explorer URL must be `https://host[:port]`
  (plain `http://` only for loopback hosts, used by tests). Userinfo
  (`user:pass@host`) is rejected outright: it can disguise a non-loopback
  host as a loopback one (`http://127.0.0.1:80@evil/`). Percent-encoded
  hosts, control characters, malformed ports, and junk after IPv6
  literals are rejected before any socket is opened.
- **No broadcast path** — the explorer REST surface is GET-only; this tool
  sends no other method. It cannot spend, stake, or broadcast, and does
  not pretend to.

Staking honesty: a watch-only light wallet cannot stake and says so. Users
who want staking point a tri7 node at their seed — the light wallet is the
*monitor*, tri7 is the *actor*.

## Design notes

- Monitoring code only: the module links against none of the
  spend/stake/sweep builders and never touches key material or wallet
  files.
- Seed input is deliberately absent — a seed in a watch tool is a
  prompt/log leak away from total compromise. If seed import is ever
  added it must be 0600 file input, never a CLI argument, never echoed.
- The offline half (deriving the address list from a BIP39 seed) is the
  air-gapped triangles-paper-wallet tool's job; both derive the canonical
  m/44'/2222' path (the "abandon…about" test vector derives the identical
  address in both tools — pinned by a unit test here).
