# Bundled i2pd — the REAL i2pd daemon shipped with the tri7 wallet

Sami, standing directive ("I2P IS FIRST-CLASS … the fleet is dual-network"):
a wallet downloaded today must dial `.b32.i2p` peers — and accept peers over
its own i2p server tunnel — on a fresh Windows machine with no local i2pd.
This payload is the **unmodified official i2pd win64-mingw build** — no
Rust-native i2p implementation, no fork, no protocol code of our own (the
NO-OWN-TOR/I2P-IMPLEMENTATIONS rule covers i2p exactly like tor).

CI (`.github/workflows/ci.yml`, windows job) downloads it, verifies the
pinned digest, and drops `i2pd.exe` + `certificates/` here; the NSIS
installer places them under the app dir as `<app>\i2pd\i2pd.exe` and
`<app>\i2pd\certificates\`, and the node supervisor
(`src/gui/node_supervisor.rs`, `I2pSupervisor`) runs it as a hidden child
with GENERATED configs (i2pd.conf + tunnels.conf in the node datadir — the
user never touches a config file), pins SOCKS 4447, and reads the stable
`.b32.i2p` destination back from the tunnel keys dat it keeps under the
node dir. A machine whose own system i2pd already answers on 4447 uses that
instead (never two).

## Pinned payload (digest computed from the exact downloaded bytes)

| field | value |
|---|---|
| version | **3.0.0** (the build proven live on sami-pc Windows; note the 3.0 conf section names `[socksproxy]`/`[sam]`/`[http]`) |
| file | `i2pd_3.0.0_win64_mingw.zip` |
| URL | `https://github.com/PurpleI2P/i2pd/releases/download/3.0.0/i2pd_3.0.0_win64_mingw.zip` |
| SHA256 (zip) | `973abdd5793e05cd21171ee701f3e2603d756543872dfb81d0a5c2d1a3ff3fb3` |
| SHA256 (i2pd.exe inside) | `6f501e1f4625ad5f3f1075e2fd4f4096aaf664386b04c526cee9b05a7301c2a2` |
| source of digest | computed from the release asset fetched on 2026-11-11; CI enforces the ZIP digest byte-for-byte |

## What ships

- `i2pd.exe` — the statically-located mingw build (no DLLs).
- `certificates/` — the official reseed + family certificates from the
  release's `contrib/certificates/`. The supervisor installs them into the
  i2pd datadir (`<datadir>\certificates`, i2pd's default lookup) so reseed
  SU3 signature verification (`verify = true` in the generated conf) works
  out of the box.

## Operational notes (each one cost a live incident)

- **Never hand i2pd 3.0.0 mingw a conf-path flag**: `--conf=` /
  `--tunnelsconf=` hang the process at startup (one thread, zero listeners,
  sami-pc 2026-11-07). The supervisor launches with ONLY `--datadir=`;
  tunnels.conf is auto-read from the datadir. On Windows NO i2pd.conf is
  written at all: i2pd's documented defaults already provide the exact
  listener set the wallet needs (socksproxy true @ 127.0.0.1:4447, SAM
  true @ 7656, webconsole true @ 7070 — all loopback), so the user's
  shared `%APPDATA%\i2pd` directory is never touched (judge r1 blocker:
  an install-to-%APPDATA% shape could clobber a user's own i2pd config).
- **Tunnel keys land in the DATADIR ROOT** (`tri7-p2p-keys.dat`) — the
  generated tunnels.conf names the keys file RELATIVELY on purpose, which
  keeps every OS path out of the generated configs (the torrc backslash
  bug class cannot arise) and makes the `.b32.i2p` address stable across
  restarts.
- AVG blocks i2pd exes on COPY but not in-place execution — run the
  extracted payload from its install dir (the installer layout does).
