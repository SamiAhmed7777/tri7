# Building tri7 from source

You do **not** need a pre-built binary. Everything the wallet and daemon need
is in this repository — build it yourself in one command per platform.

Repository: <https://github.com/SamiAhmed7777/tri7>
Version in this tree: **7.0.1** (see `Cargo.toml`)

---

## 1. Get the source

```bash
git clone https://github.com/SamiAhmed7777/tri7.git
cd tri7
```

If you downloaded a ZIP instead, unzip it and `cd` into the folder — same thing.

---

## 2. Install the Rust toolchain (once)

Install Rust with [rustup](https://rustup.rs). Rust **1.75 or newer** is
required; edition 2021.

| Platform | Command |
|---|---|
| Linux / macOS | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` then `source "$HOME/.cargo/env"` |
| Windows | download and run [rustup-init.exe](https://win.rustup.rs) |

**Windows needs the MSVC C++ toolchain** (the vendored Hash9 C code is compiled
during the build):

1. Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/)
2. Select the **“Desktop development with C++”** workload, install
3. Then run `rustup-init.exe` and accept the default `stable-x86_64-pc-windows-msvc`
   toolchain

On Linux you also need a C compiler and the usual GUI build headers:

```bash
# Debian / Ubuntu
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libxkbcommon-dev \
    libwayland-dev libx11-dev libxi-dev libxcursor-dev libxrandr-dev \
    libgl1-mesa-dev libssl-dev

# Fedora
sudo dnf install -y gcc gcc-c++ pkg-config libxkbcommon-devel wayland-devel \
    libX11-devel libXi-devel libXcursor-devel libXrandr-devel mesa-libGL-devel openssl-devel
```

On macOS: `xcode-select --install` (Command Line Tools) is enough.

---

## 3. Build

### 3a. Quick build (everything, one command)

```bash
cargo build --release --features gui --bins
```

This produces **both** the GUI wallet and the node daemon:

- Linux/macOS: `target/release/tri7_wallet_gui` and `target/release/follow_tip`
- Windows: `target\release\tri7_wallet_gui.exe` and `target\release\follow_tip.exe`

First build takes a few minutes (it compiles SQLite and the GUI stack);
later builds are incremental and fast.

### 3b. Explicit, platform-by-platform

**Windows (PowerShell, from the repo root — this is exactly what CI runs):**

```powershell
cargo build --release --features gui --bin tri7_wallet_gui   # GUI wallet
cargo build --release --bin follow_tip                        # node daemon
cargo test  --release --lib                                   # test suite
```

**Linux / macOS:**

```bash
cargo build --release --features gui --bin tri7_wallet_gui   # GUI wallet
cargo build --release --bin follow_tip                        # node daemon
cargo test  --release --lib                                   # test suite
```

**Headless server (no GUI dependencies at all):**

```bash
cargo build --release --bins
```

The GUI is behind the `gui` cargo feature; without it egui/eframe are never
downloaded or compiled.

### 3c. Cross-compiling the Windows build from Linux (optional)

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt-get install -y mingw-w64          # Debian/Ubuntu
cargo build --release --target x86_64-pc-windows-gnu --features gui --bin tri7_wallet_gui
cargo build --release --target x86_64-pc-windows-gnu --bin follow_tip
# -> target/x86_64-pc-windows-gnu/release/*.exe
```

The CI pipeline does exactly this for the Windows release artifacts, so it is
known to work; a native Windows build is still the most robust option for
end users.

---

## 4. Run what you built

### GUI wallet

```bash
./target/release/tri7_wallet_gui --wallet /path/to/wallet.dat
```

Windows:

```powershell
.\target\release\tri7_wallet_gui.exe --wallet E:\Coins\TRI\wallet.dat
```

On first open, if there is no wallet at that path, the wallet creates a new
**seed-phrase** wallet there. Everything else (node state, chain data, backups)
lives in that same folder — one folder is the whole wallet.

The GUI can run its own embedded node. To point it at a node you run yourself,
start the daemon first (below) and the wallet picks it up on `127.0.0.1:19114`
then `127.0.0.1:19112`.

### Node daemon

```bash
./target/release/follow_tip \
  --daemon <peer-host:24113> \
  --rpc-port 19112 \
  --state-dir /var/lib/tri7 \
  --forever
```

From zero, add `--sync-from-zero` (full header walk + body fetch, resumable;
the daemon checkpoints its own progress so an interrupted sync continues where
it stopped).

Check it:

```bash
./target/release/follow_tip --status
curl -s -X POST http://127.0.0.1:19112 -d '{"method":"getblockcount"}'
```

---

## 5. Where the binaries end up

| What | Linux/macOS | Windows |
|---|---|---|
| GUI wallet | `target/release/tri7_wallet_gui` | `target\release\tri7_wallet_gui.exe` |
| Node daemon | `target/release/follow_tip` | `target\release\follow_tip.exe` |
| CLI tools (`balances`, `stake_mint`, …) | `target/release/` | `target\release\` |

You can copy these two executables anywhere; the GUI looks for the daemon next
to itself. Nothing else has to be installed.

---

## 6. Troubleshooting

**`error: linker `cc` not found` (Linux)** — install `build-essential` / `gcc`.

**`error: Microsoft Visual C++ build tools not found` (Windows)** — install the
“Desktop development with C++” workload from Visual Studio Build Tools, then
re-run the build; rustup will detect it.

**Missing `libxkbcommon` / `libwayland` / `libGL` when building the GUI on
Linux** — install the packages listed in step 2; the GUI needs the windowing
headers at compile time.

**A stale daemon keeps an old `follow_tip.exe` locked on Windows** — stop it
first (`taskkill /f /im follow_tip.exe`) and rebuild.

**Slow first build** — expected: SQLite, egui and the hash library compile once
and are cached afterwards.

---

## 7. Verifying a build is sane

```bash
cargo test --release --lib          # unit suite (320+ tests)
./target/release/tri7_wallet_gui --selfcheck    # headless wallet roundtrip
```

CI runs the same on Linux, Windows and macOS for every push
(`.github/workflows/ci.yml`); if those jobs are green, a clean local build of
the same commit is expected to pass too.
