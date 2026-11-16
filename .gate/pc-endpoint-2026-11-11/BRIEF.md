# ITEM 22 — PC-ENDPOINT-PUBLISH + DNS2 CONF HYGIENE (ops) — judge brief

## What is being graded

OPS-ONLY tick, no code change, one docs commit (HANDOFF.md section). Live
changes (result: PARTIAL RECOVERY with an observed residual reachability
defect — see the verification summary):

1. **PC (sami-pc) onion endpoint — publish restored (partial recovery)** —
   tri7tor tor instance rebuilt:
   fresh bootstrap + directory-cache reset (`tor\data` renamed to
   `data.bak-item22`, reversible; HS keys in `tor\hs` untouched — the onion
   address tuu7h65j…onion is UNCHANGED and must never change without a fleet
   conf update). Result: the v3 descriptor publish machinery WORKS again —
   `handle_response_upload_hsdesc: Uploaded hidden service descriptor (status
   200 ("HS descriptor stored successfully."))` at 2026-11-11T09:18:37Z
   (evidence: torinfo-evidence.log). End-to-end dial proof from DNS2 through
   the whole tor path into tri7's own listener at 09:47-09:48Z
   (`SOCKS5 request granted` + PC daemon journal
   `inbound handshake 100.85.236.10:50070 failed: inbound handshake: no
   version from client` — the backend ACCEPTED the tor-delivered connection;
   the "failure" is my curl probe not speaking the tri7 handshake).
2. **DNS2 addnode conf hygiene** on the REAL config path
   `/etc/tri7/triangles.conf` (backup `.bak-20261111-item22`): retired the 3
   cleartext addnodes (ignored by tri7d with a live journal line every boot),
   the dead samihost onion i6tk7…:24112 (SOCKS reply 1, descriptor not found,
   zero connections ever), the legacy i2p lfuojj…:24112 (SOCKS reply 4 host
   unreachable, zero connections ever; the same destination was already
   retired fleet-wide in the PC conf on 2026-11-07) — each with a dated,
   evidence-bearing comment. Kept `addnode=mb3ox52u…b32.i2p:24113` (the live
   PC i2p bridge). tri7d restarted via the health-timer dance; boot journal
   has EXACTLY ONE addnode line (accepted (i2p)), ZERO ignore-warnings, ZERO
   dead dials; staking enabled and advancing; tip advancing; peers returned.
3. **Decoy conf banner**: `/root/.triangles/triangles.conf` now carries a
   dated DECOY NOTICE header (tri7d reads/persists /etc/tri7/triangles.conf
   only — the trap that cost a sibling lane a judge round on 2026-11-11
   05:1x-05:4xZ). Backup kept beside it.
4. **Bounded PC verify window** (disclosed, restored): the daemon tasks are
   DISABLED by operator/lane action (~06:14Z, tri7d2 Last Result 1 — its last
   run died silently mid-tips-advancing; GUI-weld 7.0.3 context). To prove
   the endpoint end-to-end without touching the operator's toggles, a ONE-OFF
   task `tri7d2-verify` ran a byte-identical copy of ft_state2.bat for a
   bounded window, then the daemon was killed, the task deleted, and all
   temp files removed. Pre-tick process state restored (tor + i2pd + GUI
   running; no tri7 daemon; tri7d2/d3/d/watchdog still disabled).

## Root cause of the "dead" published onion (the queue item's two hypotheses resolved)

- torrc-vs-bind mismatch: **DISPROVEN for the current configs** — torrc
  `HiddenServicePort 24113 100.85.236.10:24113` and the daemon bat's
  `--listen-bind 100.85.236.10` MATCH exactly, and the backend path works:
  tor's service side established the backend connection and tri7's listener
  accepted it (the 09:48:41Z inbound-handshake line), and i2pd (also SYSTEM)
  dials the same tailscale IP:port successfully (the live i2p bridge).
## Observed failure and bounded recovery (measured facts vs hypothesis, stated separately)

- The descriptor was unfetchable for ~40h+ (MEASURED: 0 INTRODUCE2 cells
  across 40h+ of heartbeats = no connection ever accepted; dials from two
  independent clients 404'd/timed out), while the tor instance's directory
  fetch volume had collapsed ~20x since Nov 9 (MEASURED). The pre-reset
  upload-path behavior and the causal chain from the weakened
  directory-cache/guards to that failure are a HYPOTHESIS (pre-reset
  info-level upload logs were not captured); the fresh-cache reset is the
  MEASURED fix lever.
- An OBSERVED residual reachability defect: after intro-point changes the
  descriptor re-upload is deferred (log_cant_upload gates +
  hs_service_dir_info_changed; the specific delay attribution is an
  INFERENCE from temporal association), so a dial failed at 10:11Z after the
  09:56 restart rebuilt the intro set and recovered by 10:16-10:19Z — ONE
  failure/recovery interval observed; periodicity unproven. Within one
  healthy window (09:41-09:48Z) the FULL path was proven end-to-end (see
  the verification summary below).

## Verification summary (all timestamped in evidence-timeline.md; dial evidence
## in evidence-dials.txt — labeled reconstructed vs raw; the key early
## observations are RECONSTRUCTED summaries, the 10:37-10:38Z pair is RAW)

- BEFORE: DNS2→PC onion dial hangs >30-40 s unbounded (08:52Z, 08:41Z); PC
  self-dial times out; 0 INTRODUCE2 cells across the previous tor session
  (no connection ever ACCEPTED — measured). The descriptor being unfetchable
  is measured; the pre-reset upload-path failure MECHANISM is an INFERENCE
  (pre-reset info-level upload logs were not captured).
- AFTER: descriptor uploaded 200 OK 09:18:37Z; DNS2 dial `SOCKS5 request
  granted` in ~4 s (09:41Z, 09:47:5xZ); tor delivered the stream to tri7's
  listener (`inbound handshake 100.85.236.10:50070` 09:48:41Z).
- Observed residual reachability defect: dial failed again at 10:11Z
  (temporal association with the 09:56 restart's intro rebuild; the
  dirty-descriptor attribution is an INFERENCE — an earlier draft's
  "10:24Z failure" claim is WITHDRAWN as mistaken; the 10:24Z log lines are
  unrelated claim-divergence output), recovered by 10:16-10:19Z, and a
  fresh [RAW] dial reached the production listener at 10:37-10:38Z.
- DNS2 conf hygiene verified live (raw boot-scoped journal for the latest
  post-hygiene boot: exactly ONE addnode line, zero addnode ignore-warnings
  ("ignored: only …" count 0), zero matches for the
  explicit dead-dial filter —
  evidence-dns2-journal.txt; staking enabled with attempts advancing in two
  complete raw RPC captures 180 s apart (1,986,189 → 2,074,869) —
  evidence-dns2-status.txt; single-snapshot facts only).
- PC end-state: tri7tor RUNNING (fixed endpoint, notice-level logging
  restored via ControlPort SETCONF), i2pd RUNNING (i2p endpoint alive
  throughout), tri7 daemon tasks still DISABLED as the operator set them,
  GUI task relaunched (it had terminated mid-window, Last Result 267014).

## What I did NOT do (deliberately)

- Did NOT re-enable tri7d2/tri7d3/tri7d/tri7d-watchdog (operator/lane set
  them disabled ~06:14Z; the 7.0.3 tor-bundle GUI supersedes this stack).
  NOTE (supersede — see evidence-window.md): at 10:16:06Z an EXTERNAL hand
  re-enabled and started tri7d2 on its own; this tick never touched the
  toggle and left it running as found. That restoration also gave the torrc
  backend target its permanent listener back.
- Did NOT change the PC's onion address or keys.
- Did NOT touch the fleet's chain state; the PC's disconnected-window solo
  branch (h2209617..24, heavier by work) is being reconciled by the deployed
  judges themselves (item-17/21 machinery, live — DNS2/PC mutual
  claim-divergence recons observed in both journals during the window).

## Judge round ledger (r1 D 6c4lr6d7 → r2 C 5lke5vuz → r3 C bnzkibc5 → r4 C ffn6e2ek → r5 C vdctouq4 → r6 C xa7sgjxq → r7 C 3c6dxqqg → r8 C tu7ar6in; round-9 re-grade below)

### Round 1 (D urn:ump:6c4lr6d73kpfowitfivtweic33hrzl67ftpqebby3q7xaaic5dca) — blockers taken

1. RPC credential redacted from etc-conf-before/after.conf (live value exists
   only in the 0600 operational file; NOT rotated — nothing left the trusted
   boundary; the no-rotation-without-consulting rule stands).
2. Raw timestamped dial transcripts + matched daemon journal lines added
   (evidence-dials.txt), including a fresh 10:38:13Z live pair against the
   production tri7d2.
3. Outcome recast as PARTIAL RECOVERY: the chronic never-publishes condition
   is fixed (uploads 200 OK, granted dials, backend accepts);
   reachability still oscillates around intro rotations (bounded ~tens of
   minutes) — NOT claimed as a stable always-on repair.
4. Cache/guard degeneration recast as hypothesis; the measured facts
   (failed-before / succeed-after) are stated separately.

## Evidence files in this bundle

- evidence-timeline.md — full timestamped narrative
- torinfo-evidence.log — tor info-level log (the smoking-gun
  log_cant_upload_desc + the 200 OK upload + purpose-20 circuit storms)
- hscheck.ps1 / setinfolog.ps1 / setnolog.ps1 — ControlPort probes
  (circuit-status HSSI_ESTABLISHED intros; log-level toggles)
- evidence-window.md — post-brief addendum (final dial states + fleet notes)
