# BRIEF — tri7 lane `lane/i2p-stack` (I2P supervisor stack, Tor parity + Kimi audit fixes)

Artifact kind: code. Base: master `06360e4`. Worktree: `/root/tri7-lanes/i2p-stack`
(branch `lane/i2p-stack`). Everything below is scoped to that worktree; the
parent repo `/root/tri7` was never touched.

TREE SHAPE / PROVENANCE: `lane.diff` is `git diff master -- <explicit file
list>` (8 files: 7 modified + 1 new README, intent-to-add) captured in the
worktree whose HEAD == the base commit `06360e4` — i.e. the bundle
proves base + diff; the graded bytes become the lane commit only AFTER this
gate passes (the lane commit is made on `lane/i2p-stack` post-grade, never
merged to master by this lane). The gate log's HEAD line equals the base
for the same reason.

## Scope

Two mandates landed in one lane:

1. **Wallet-native I2P stack at Tor parity** (operator brief + scope
   expansions): the wallet SHIPS/MANAGES the real i2pd daemon exactly like
   `TorSupervisor` manages bundled tor — bundled payload first, system
   binary fallback, generated configs (user never touches a config file),
   pinned SOCKS port, stable server tunnel, offline `.b32.i2p` destination
   readback, GUI chip parity, embedded seed floor for both transports.
2. **The 12-finding Kimi transport audit** (`/tmp/kimi-i2p-audit/FINDINGS.md`,
   read in full): every goal-blocking HIGH/MED item is fixed in this lane;
   each fix carries a test where testable.

## What changed (file → why)

- `src/p2p.rs` — wire layer is now dual-stack:
  - `i2p_b32_label()` (base32 of the 32-byte destination hash) and
    `WireAddr::i2p_from_hash()` / `from_i2p_label()` / `i2p_hostname()`:
    GarliCat ip prefix + full hash in `tor_key` + `is_i2p = true`
    (audit HIGH-4b/MED-4c).
  - `store_key()` gained the i2p arm (returns the dialable `.b32.i2p`
    hostname); `from_store_key()` parses `.b32.i2p` back — getaddr can
    re-encode wire-learned i2p peers (HIGH-4b/MED-4c).
  - Tests: RFC-4648 pin vectors, full label→wire→hostname→store→getaddr
    round trip incl. `is_i2p` across BOTH wire shapes, the LIVE DNS2 fleet
    identity pinned (`ztj5lcg4…`), junk-label rejection.
- `src/peers.rs` — `merge_addr` accepts tor_v3 AND i2p gossip; cleartext IP
  gossip stays rejected (audit HIGH-4a). Test: i2p merges keyed by b32
  hostname, re-serves over getaddr, lands in the dial plan; clearnet
  rejected; tor unchanged.
- `src/node_embedded.rs`:
  - `DEFAULT_PEER_SEEDS` — the fleet seed floor (DNS2 onion from
    `/var/lib/tor/triangles/hostname`, DNS2's LIVE i2p tunnel identity
    derived from `/var/lib/i2pd/tri7d-p2p-keys.dat` and cross-checked
    against i2pd's own state, two clearnet bootstraps). Augmented into every
    boot's target list (dedup by endpoint, user targets first) — HIGH-5.
  - `overlay_addr_me(state_dir, listen_port)` — self-advertise: version
    `addr_me` carries the i2p destination (`i2p_destination.txt`, written by
    the supervisor) or the onion hostname when the node LISTENS; otherwise
    the historical 127.0.0.1 placeholder bytes (byte-identical to pre-lane)
    — MED-4d. Outbound (`connect_and_handshake`) and inbound
    (`accept_handshake`) both advertise; `onetry` keeps the placeholder
    (disclosed in-code).
  - Per-transport dial windows (MED-3a): SOCKS read 90s / handshake 60s for
    i2p (tunnel builds are slow), clearnet/tor UNCHANGED (30s/20s).
  - `credit_body_miss_round` skips i2p-labeled peers (MED-3b): the 45s
    getdata age-out/requeue still applies to everyone; i2p sources are
    never DEMOTED for a tunnel that is still building. Clearnet/tor
    demotion unchanged.
  - `transport_of_advertised` — inbound peers classify by their OWN
    advertised addr shape instead of the hardcoded Clearnet (LOW-2b);
    placeholder stays honest-Clearnet.
- `src/gui/node_supervisor.rs` — `I2pSupervisor` (the tor lane's twin):
  - `I2pSpawnRequest`, `I2pStatus::{System,Bundled,Unavailable}` + label()
    + socks_port(); `new(log_path)`, `status()`, `ensure(node_dir,
    allow_spawn)`, `socks_port()`, `destination()`.
  - Binary lookup mirrors `bundled_tor_exe` (`i2pd\i2pd(.exe)` beside the
    GUI exe / wallet / node dir), falling back to a system i2pd
    (`/usr/sbin|i2pd`, `/usr/local/bin`) on unix — MED-6b.
  - Generated configs (validated LIVE on i2pd 2.56 on this host — all three
    listeners bound, server tunnel created, keys written — and byte-shape
    consistent with the official 3.0.0 win64-mingw example conf):
    - `i2pd.conf` (non-Windows ONLY, passed via `--conf`):
      `[socksproxy]` (pinned 4447) + `[sam]` (7656) + `[http]` webconsole
      (7070), ALL loopback-only; relay roles disabled; reseed verification
      on. NO filesystem paths in the file by design.
    - `tunnels.conf`: `[tri7-p2p]` server tunnel → `127.0.0.1:<p2p_port>`
      with `keys = tri7-p2p-keys.dat` RELATIVE (lands in the DATADIR ROOT —
      the sami-pc layout), so the config carries NO OS path at all (the
      torrc backslash-escape bug class cannot arise) and the destination
      identity is STABLE across restarts. The conf lives IN the datadir;
      i2pd auto-reads it from there (live-verified). On WINDOWS there is
      NO i2pd.conf and NO `%APPDATA%` write at all (judge r1 blocker):
      i2pd's documented defaults already carry the exact loopback listener
      set, the spawn passes ONLY `--datadir=` (the mingw-3.0.0
      conf-flag-hang lesson, regression-pinned), and the user's shared
      i2pd directory is never touched; each wallet keeps a node-local
      datadir so multiple wallets cannot conflict.
  - SOCKS port PINNED at 4447 with the already-answer probe first
    (MED-6c — a respawn on a fresh free_port() would orphan the running
    node's startup argv; system i2pd adoption mirrors tor's 9050 rule).
  - Supervision checklist replicated from tor: hidden child
    (CREATE_NO_WINDOW), stdout/stderr tee into node.log, kill-on-Drop,
    `spawn_gave_up` latch, bounded 60s SOCKS wait.
  - Destination readback: `b32_from_destination_keys()` derives
    `base32(sha256(dest)) + ".b32.i2p"` OFFLINE from the keys dat (no
    webconsole dependency); a fresh destination emits the named log line
    `I2P SERVER TUNNEL READY: <b32>` into node.log and writes
    `<state>/i2p_destination.txt` (`host:port`) — the exact file the node's
    `getlocalinfo` ALREADY serves (`src/rpc.rs:2391`), which is what lights
    the GUI chip. DERIVATION PROVEN ON TWO LIVE IDENTITIES: DNS2's fleet
    tunnel (`ztj5lcg4…`) and a throwaway probe instance where the derived
    address equaled i2pd's own logged address byte-for-byte.
  - `ensure_i2p_conf_lines` — the conf gains the fleet i2p seed as an
    `addnode=` line when i2p is live (mirrors the tor onion-seed line,
    idempotent) — HIGH-5's wallet-path half.
  - `ensure_running`: i2p ensured BEFORE the node spawns; the node argv
    carries `--i2psocks 127.0.0.1:4447`; when the server tunnel is live the
    `--no-listen` placeholder is replaced by `--listen-bind 127.0.0.1
    --listen-port 24113` (i2pd forwards `<b32>:24113` into the loopback
    listener — the wallet becomes i2p-reachable without exposing anything
    on the LAN). Sequencing disclosed in-code: the tunnel usually finishes
    building after the node starts, so inbound activates on the next
    node restart; DIALING works immediately (i2pd binds SOCKS at startup).
- `src/gui/app.rs` — status-bar I2P chip tor-parity (honest supervisor
  label, muted when live / amber when unavailable, hover text), drawer
  "needs i2pd" placeholder replaced with the honest "tunnel not up yet"
  text. The drawer's address flows via `getlocalinfo` (`local_i2p`) exactly
  as before — now the value is real.
- `.github/workflows/ci.yml` — `Fetch bundled i2pd (pinned 3.0.0 + SHA256
  gate)` step: official `i2pd_3.0.0_win64_mingw.zip` from the PurpleI2P
  release, ZIP digest `973abdd5…3fb3` enforced (job fails on mismatch),
  `i2pd.exe` (`6f501e1f…2a2` inside the same zip) + the official reseed
  certificates into `deploy/windows/i2pd-bundle/`; the portable zip carries
  `i2pd\i2pd.exe` + certificates beside tor (asset NAMES unchanged so the
  updater's prefix-matching fixtures stay valid) — MED-6b.
- `deploy/windows/tri7-wallet.nsi` — installer stanza mirroring the tor
  one: `$INSTDIR\i2pd\i2pd.exe` + license + certificates; uninstaller
  cleanup. `deploy/windows/i2pd-bundle/README.md` — the pin/provenance doc
  (mirror of tor-bundle's).

## Audit-disposition notes (12 findings)

- 4a/4b/4c/5/3a/3b/6a/6b/6c/2b: fixed + tested as above.
- MED-getlocalinfo ("the node has NO such RPC"): the audit's ONE
  misdiagnosis — `getlocalinfo` EXISTS in the embedded node's native RPC
  (`src/rpc.rs:2371`, allow-listed at `src/rpc.rs:93`) and already reads
  `i2p_destination.txt`/`i2p/hostname` from the state dir. The gap was
  upstream of it: nothing wrote those files. This lane's supervisor write
  closes it; no RPC change needed (GUI mock `tests.rs:4519` unchanged).
- smsg (audit section 1): CLEAN — no changes; smsg rides the P2P session
  layer, transport-blind by design (verified by the audit + this lane's
  store/dial work now actually REACHING i2p peers).

## Evidence integrity

- The generated i2pd configs were validated on a REAL i2pd 2.56 instance on
  this host (isolated datadir, then killed): `[socksproxy]`/`[sam]`/`[http]`
  accepted, SOCKS 14447/SAM 17656/webconsole 17070 bound, 1 server tunnel
  created, keys dat written to the datadir root (679 B), and the b32
  derived from that dat equaled the address i2pd itself logged. Reseed was
  blocked on this host's network, so the probe bootstrapped from a READ-ONLY
  copy of the live system netDb into its own isolated datadir (the live
  daemon's files were never touched; probe dirs deleted after).
- The i2pd 3.0.0 win64-mingw zip digest was computed from the actual
  release asset downloaded on this host (2026-11-11) — not quoted from a
  webpage.

## Gates (see full-gates.log for complete output; CARGO_TARGET_DIR=/root/tri7-lanes/target-i2p)

1. `cargo test --lib` — see log (GATE1_RC=0)
2. `cargo test --release --lib` — see log (GATE2_RC=0)
3. `cargo check --bin tri7_wallet_gui --features gui` — see log (GATE3_RC=0)
   (GUI compile gate: the lib suite cannot catch app.rs errors)
4. `cargo build --release --target x86_64-pc-windows-gnu --features gui
   --bins` — see log (GATE4_RC=0)

Test counts quoted from the log: debug `759 passed; 0 failed; 1 ignored`,
release `759 passed; 0 failed; 1 ignored` (baseline 739 + 20 new).

## Judge round 2 (D, urn:ump:poq62wmfsvmz6juhgzn3aqu24t7ic5fs4bg5jx2zpcko22gzpouq) — disposition

- BLOCKER "own live child killed on every ensure": REAL and subtle. With
  the PINNED port (MED-6c), the spawned child binds 4447 — the same port
  the system probe uses — so probe-before-child-check reclassified our own
  daemon as System and killed it on every poll tick (tor never hits this
  because its bundled child takes a fresh free port). FIXED: ensure()
  checks the tracked live child FIRST; the foreign-system adoption probe
  runs only when we have no live child. Regression test in the judge's
  exact demanded shape: `i2p_own_child_is_never_reclassified_or_killed_across_ensures`
  — a stub listener that STAYS BOUND across four ensure calls (the r1
  one-shot stub was what masked the bug), asserting Bundled (never
  reclassified), zero kills, zero re-spawns, same pid, destination intact.
  The stub now keeps the port bound like real i2pd in ALL lifecycle tests.
- BLOCKER "stale wallet destination advertised under an adopted foreign
  system i2pd": REAL. A previous session's keys dat does not make an
  address live in a foreign daemon. FIXED: System adoption performs NO
  destination refresh, drops any in-memory destination with a named log
  line, and removes the stale `state/i2p_destination.txt` so the node's
  live per-handshake identity read (`overlay_addr_me`) and getlocalinfo
  stop advertising immediately. Regression test:
  `i2p_system_adoption_never_claims_a_stale_wallet_destination`.
- The bundled-spawn lifecycle test assertions were also made
  platform-conditional (i2pd.conf is generated on non-Windows only).

## Judge round 1 (D, urn:ump:haloko2vfmz4o5ejfnxdamjqon24v7kulm67zs4ufdyz3p4rmqiq) — disposition

- BLOCKER "spawn argv carries the exe twice": REAL production defect (i2pd
  aborts an unknown positional with "unrecognised option" — the bundled
  launch path could never have worked; the spawn_override test stubs and
  the pure-builder test both missed the Command wiring). FIXED:
  `i2pd_spawn_argv` now returns ARGUMENTS ONLY and `spawn_i2pd` passes
  `Command::new(exe).args(args)`; the argv test pins "exe must not be an
  argument" and the production-shaped shapes.
- BLOCKER "%APPDATA%\i2pd overwrite": REAL design risk. FIXED by
  REMOVING the Windows conf install entirely: i2pd's DOCUMENTED defaults
  (official example conf shipped in the pinned payload) already carry the
  exact listener set the wallet needs — socksproxy true @ 127.0.0.1:4447,
  SAM true @ 7656, webconsole true @ 7070, ALL loopback — so Windows gets
  `--datadir=` + datadir tunnels.conf ONLY and the user's shared
  `%APPDATA%\i2pd` is never read or written; multiple wallets use their
  own node-local datadirs. i2pd.conf (explicit hardening: disabled relay
  roles) is still generated and passed via `--conf` on non-Windows.
- BLOCKER "run the bundled Windows i2pd payload through an integration
  test": NOT executable from this lane — there is no Windows runner here
  (the task's gate set ends at the windows-gnu cross build, and this lane
  must not deploy/restart anything). Honest substitutes shipped: the
  launch-wiring defect the judge found is fixed + pinned by a test; the
  generated configs were validated end-to-end on a REAL i2pd 2.56 on this
  host (listeners bound, tunnel created, keys written, b32 readback equal
  to i2pd's own logged address); the CI windows job runs the pinned fetch
  + NSIS on the real msvc runner at the next push. Stated plainly: NO
  live-Windows execution claim is made anywhere in this brief.

## Known limits (disclosed, not silent)

- Bundled i2pd on Windows: CI fetch is wired and pinned, but no Windows
  runner execution happened in this lane (the windows-gnu cross-build gate
  compiles the GUI; the NSIS/i2pd steps run on the CI's real msvc runner on
  next push). The Windows launch contract (args-only argv, --datadir-only
  spawn, defaults-over-shared-config) is unit-tested; the mingw conf-flag
  hang + datadir keys layout are the fleet-proven sami-pc lessons.
- Inbound-i2p activation for the GUI node lands on the node restart AFTER
  the tunnel first comes up (sequencing disclosed above).
- `.i2p` DNS-style names remain conf-only dial targets (no destination hash
  → no wire form) — documented in `from_store_key`.
