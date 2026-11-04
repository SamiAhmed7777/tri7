# tri7 — Cryptographic Triangles (TRI) modern client + GUI wallet

> **Build it yourself:** no pre-built binary is required. See
> **[BUILDING.md](BUILDING.md)** for step-by-step instructions per platform
> (clone -> install Rust -> one build command). Everything needed is in this
> repository.

A from-scratch Rust reimplementation of the Triangles full-node client:
bit-exact chain replay, PoS staking, native JSON-RPC, P2P (clearnet/Tor/I2P),
secure messaging (smsg), and a desktop GUI wallet carrying the original
Triangles identity.

## Repository layout

```
src/             core library (consensus, wallet, spend, stake, smsg, rpc, blockindex)
src/bin/         binaries: follow_tip (tri7d daemon), tri7_wallet_gui, balances, stake_mint, ...
src/gui/         GUI wallet (egui) — gated behind the `gui` cargo feature
tests/           integration test suites
docs/            specs (wallet format, PoS consensus)
deploy/          systemd units + installers (DNS2, samihost variants)
vendor/          vendored C hash implementations (Hash9 sphlib)
```

## Quick start (build from source)

```bash
git clone https://github.com/SamiAhmed7777/tri7.git
cd tri7
cargo build --release --features gui --bins
```

That is the whole build. Full platform notes, Windows toolchain setup,
cross-compiling and troubleshooting: **[BUILDING.md](BUILDING.md)**.

## Prerequisites (all platforms)

- **Rust** 1.75+ — install via [rustup](https://rustup.rs):
  - Linux/macOS: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
  - Windows: run `rustup-init.exe` (default MSVC toolchain; needs
    [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/) with
    "Desktop development with C++")
- A C compiler for the vendored Hash9 build (cc/gcc/clang on Linux/macOS, MSVC on Windows)
- ~2 GB disk for build artifacts, ~1 GB for chain data

## Linux / macOS

```bash
# daemon + CLI tools (headless, no GUI deps)
cargo build --release --bins

# GUI wallet (egui)
cargo build --release --features gui --bin tri7_wallet_gui

# run the full test suite
cargo test --release
```

Binaries land in `target/release/`: `follow_tip` (the daemon), `tri7_wallet_gui`,
`balances`, `stake_mint`, validation tools.

Headless servers: build without `--features gui` — the GUI code is feature-gated
and adds egui/eframe dependencies only when requested.

## Windows

Full Windows walk-through (rustup + MSVC Build Tools + commands): see
**[BUILDING.md](BUILDING.md#2-install-the-rust-toolchain-once)**.

### Native build (recommended — this is what CI uses)

```powershell
# in PowerShell, from the repo root
cargo build --release --bins                  # daemon + tools
cargo build --release --features gui --bin tri7_wallet_gui   # GUI wallet
cargo test --release --lib                    # tests
```

Output: `target\release\follow_tip.exe`, `target\release\tri7_wallet_gui.exe`.

### Cross-compiling from Linux (alternative)

```bash
rustup target add x86_64-pc-windows-gnu
# Debian/Ubuntu: sudo apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu --bins
# -> target/x86_64-pc-windows-gnu/release/follow_tip.exe
```

The GUI cross-build also works under the gnu toolchain; if a graphics-backend
issue appears, prefer the native Windows build (or CI artifacts).

## macOS

```bash
cargo build --release --bins
cargo build --release --features gui --bin tri7_wallet_gui
cargo test --release --lib
```

## Running

### Daemon (tri7d)

```bash
./target/release/follow_tip \
  --daemon <peer-addr:port> \        # repeatable; first entry replaces the default
  --rpc-port 19112 \                 # native JSON-RPC, loopback-only
  --conf /etc/tri7/triangles.conf \  # optional; rpcuser/rpcpassword, stake=0|1, tor/i2p targets
  --state-dir /var/lib/tri7 \        # tipstate.json + block index + wallet snapshots
  --forever
```

- `stake=1` in the conf enables continuous live staking (wallet keys required).
- Tor/I2P: `.onion` targets dial via tor SOCKS (127.0.0.1:9050), `.b32.i2p`
  via i2p SOCKS (127.0.0.1:4447). Inbound Tor needs a HiddenServiceDir pointing
  at the P2P port (see `deploy/tri7d.service` for a worked example).
- `--status` prints tip/peer/health JSON (exit 0 healthy, 3 degraded).

### GUI wallet

```bash
./target/release/tri7_wallet_gui            # opens the wallet window
./target/release/tri7_wallet_gui --selfcheck   # headless state-machine roundtrip
```

Default wallet path `/root/.triangles/wallet.dat` (read-only); the RPC default
endpoints are `127.0.0.1:19112` then `127.0.0.1:19114`. Sending is a
two-step flow: **Sign — build transaction** (offline, real keys) then
**Broadcast** (relays via tri7d). The Messages page is the smsg secure
messaging client; the Explorer queries the local node only — never an
external service.

### systemd

See `deploy/tri7d.service`, `deploy/tri7d-health.{service,timer}`, and
`deploy/install_tri7d.sh` (Linux installer: user, dirs, units, healthcheck).

## CI

Every push builds and tests on Linux, Windows, and macOS
(`.github/workflows/ci.yml`); Windows runs produce `follow_tip.exe` +
`tri7_wallet_gui.exe` artifacts on the Actions page.

## Building from source

If you would rather not use a release artifact, this repository builds
everything from source with a standard Rust toolchain — GUI wallet and node
daemon alike. Start at **[BUILDING.md](BUILDING.md)**.

## Notes

- Wallet files are opened **read-only**; the GUI's "New Address" creates keys
  in a separate 0600 keystore (`~/.tri7_gui/keystore.json`), never writes
  wallet.dat.
- The chain replay is byte-exact with the deployed legacy client, including
  era-specific validation envelopes (see `docs/pos-consensus-spec.md`).
