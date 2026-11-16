# ITEM 22 PC-ENDPOINT-PUBLISH + DNS2 CONF HYGIENE — evidence timeline (live tick 2026-11-11 08:2x-10:0xZ)

## Starting state (measured 08:22-08:35Z)
- DNS2 (local): healthy, sole heavy staker, tip 2209589, staking enabled, 3 peers
  (out:ra32xf5og…=Pi tor, 2x in:127.0.0.1). /etc/tri7/triangles.conf addnode set dirty:
  3 cleartext addnodes (100.98.140.83:24112, 100.85.236.10:24112, 100.106.76.60:24113)
  IGNORED by tri7d each boot ("only .onion / .b32.i2p accepted — no cleartext P2P",
  journal 07:25:52Z); dead samihost onion i6tk7…:24112 (accepted but SOCKS reply 1,
  descriptor not found, zero connections ever); legacy i2p lfuojj…:24112 (accepted but
  SOCKS reply 4 host-unreachable, zero connections ever; same destination retired in the
  PC conf 2026-11-07); live bridge mb3ox52u…b32.i2p:24113 (the PC i2p endpoint, ALIVE).
- sami-pc: tri7d2 + tri7d3 + tri7d + tri7d-watchdog DISABLED (operator/lane, ~06:14Z;
  tri7d2 Last Result 1 after its last run died silently at 06:14:30Z mid-tips-advancing);
  tri7gui-run RUNNING (GUI PID 34972, stuck SYN_SENT to 127.0.0.1:19114 — no node to
  talk to); tri7tor RUNNING since 11-09 16:56 PST; i2pd RUNNING (PC i2p endpoint alive);
  NO follow_tip process, NO 24113 listener, NO 19114 listener.

## PC onion diagnosis (observed chain; causal attributions marked as such)
1. BEFORE control dial DNS2→tuu7h65j…onion:24113 via socks 9050: HANG >30s (timeout).
   PC self-dial via own tor: TIMEOUT (25s). => the descriptor was unfetchable
   (MEASURED at these timestamps); that a descriptor was never successfully
   PUBLISHED is an INFERENCE (pre-reset upload-side logs were not captured).
2. tor.log (PST-local): heartbeats since 11-09 16:56 PST show "Our onion service
   received 0 v3 INTRODUCE2 cells and attempted to launch 0 rendezvous circuits" —
   NOBODY EVER CONNECTED to this onion across the whole previous tor session.
   Repeating "Closed 1 streams for service [scrubbed].onion … resolve failed … No more
   HSDir available to query" clusters = the daemon's periodic dials to DEAD onions in
   its conf/peer store (old-fleet onions jbpfhe/6ygpph/nawqqo etc.).
3. tri7tor restart (schtasks end+run, 08:38Z): fresh bootstrap 100% at 08:40:15Z.
   ControlPort circuit-status probe (hscheck.ps1): 6x PURPOSE=HS_SERVICE_INTRO
   HS_STATE=HSSI_ESTABLISHED REND_QUERY=tuu7h65j… built 08:40:15-17Z — intro points
   HEALTHY. Client paths healthy too (daemon dials to DNS2 onion + Pi onion succeeded
   08:42:07/08:42:21Z in its verify window).
4. Info-level logging via ControlPort SETCONF (setinfolog.ps1) exposed the
   post-reset upload gating (KEY EVIDENCE for the current process — it does
   NOT establish the prior 40h upload history):
   Nov 11 00:55:42 PST [info] log_cant_upload_desc: Service can't upload its CURRENT
   descriptor: Next upload time is 1794390455, it is now 1794387342. (= 09:47:35Z vs
   08:55:42Z — deferred ~52 minutes), and its NEXT descriptor at 1794391154 (= 09:59:14Z,
   699s before the following boundary — tor's normal next-period pre-upload slot).
   The service re-tries to upload ~1/sec (591 suppressed messages per 600s) but its own
   next_upload gate defers everything to the time-period boundary. Consistent with the
   observed unfetchability during this window (INFERENCE: dial failures are consistent
   with the descriptor not being on HSDirs at those times; the gate itself is measured).
5. torrc-vs-bind hypothesis from the queue item text: DISPROVEN for the current configs —
   torrc HiddenServicePort 24113 100.85.236.10:24113 and the daemon bat's
   --listen-bind 100.85.236.10 MATCH exactly. Cross-evidence the backend path works:
   i2pd (also SYSTEM) dials the same 100.85.236.10:24113 successfully (the live i2p
   bridge), and DNS2's own TCP probe to 100.85.236.10:24113 was OPEN (sibling lane
   .gate/dns2-addnode-port-fix-2026-11-11/evidence, 05:1x-05:4xZ window when tri7d2 ran).
6. PC daemon verify window (one-off schtasks tri7d2-verify → ft_verify.bat, a copy of
   ft_state2.bat — tri7d2's disabled state NOT touched): daemon booted 08:41:31Z,
   addnodes accepted, handshaked DNS2 onion + Pi onion, chain slots seeded 08:42:23Z,
   then DIED SILENTLY ~08:42-08:45Z (no error line, no listener phase reached —
   the item-23 chronic hard-exit pattern REPRODUCED ON DEMAND; task Last Result -1;
   WER LiveKernelEvent records ~00:34 PST nearby). NO AVG pop/block evidence found in
   Application log EventID 1000/1001/1002 within the window.

## DNS2 conf hygiene (landed + live-verified 09:04-09:10Z)
- Backup: /etc/tri7/triangles.conf.bak-20261111-item22 (byte-exact pre-edit copy).
- Edit (python replace, diff-verified): retired the 3 cleartext addnodes + dead
  i6tk7 onion + dead lfuojj i2p, each with a dated evidence-bearing comment; kept
  addnode=mb3ox52u…b32.i2p:24113 (the live PC i2p bridge). stake=1 and all other
  lines untouched.
- Decoy banner: /root/.triangles/triangles.conf now carries a dated DECOY NOTICE
  header (tri7d reads/persists /etc/tri7/triangles.conf only); backup
  triangles.conf.bak-20261111-item22 beside it.
- Disclosure: the sibling lane's .gate/dns2-addnode-port-fix-2026-11-11 r2 edit had
  set addnode=100.85.236.10:24113 (cleartext — inert vs the current binary); this
  item's cleanup retires that line with its evidence comment; their bundle explicitly
  logged the PC onion as the separate follow-up defect (= this item).
- Restart dance: tri7d-health.timer + tri7d-health.service stopped, tri7d stopped
  (SIGTERM slow ~60s, known behavior), started 09:05:10Z. Boot journal: EXACTLY ONE
  addnode line (mb3ox52u accepted (i2p)), ZERO "ignored" warnings, ZERO dead-dial
  lines. Post-restart: staking enabled with attempts advancing in paired
  180s-apart raw samples (evidence-dns2-status.txt); peers returned; the
  raw boot-scoped journal (exactly ONE addnode line, zero ignores, zero
  dead dials) is evidence-dns2-journal.txt.

## Endpoint end-to-end result (FINAL, supersedes the PENDING line above)

- 09:18:37Z descriptor upload 200 OK (torinfo-evidence.log; tor logs are
  PST-local — the log line reads 01:18:37 PST = 09:18:37Z).
- 09:41Z + 09:47-09:48Z dials: SOCKS5 request granted in ~4 s; tor delivered
  the stream into tri7's listener (PC journal 09:48:41Z `inbound handshake
  100.85.236.10:50070 failed: inbound handshake: no version from client` —
  the curl probe does not speak the tri7 protocol).
- 10:11Z dial: general failure — OBSERVED temporal association with the
  09:56 tri7tor restart rebuilding the intro set; the dirty-descriptor
  re-upload-delay ATTRIBUTION is an INFERENCE (the failed dial itself is a
  reconstructed summary), not directly established.
- 10:16:08Z an INTRODUCE_ACK success line appeared in the scrubbed tor info log (observation — the scrubbed log does not name the onion, so it is not attributable to this service specifically);
  10:19:11Z dial granted again; at 10:38:13Z another fresh dial delivered
  into the (externally re-enabled) production tri7d2 listener
  (`inbound handshake 100.85.236.10:64092`, state2.log).
- Honest classification: the CHRONIC condition ("never publishes, never
  accepts a connection in 40h") is FIXED — the descriptor lifecycle now
  works (uploads succeed, intros ACK, dials complete to the backend).
  One full failure/recovery interval was observed around the 09:56 restart
  (fail 10:11Z → recovered 10:16Z); this is an OBSERVED RESIDUAL
  REACHABILITY DEFECT whose periodicity and long-term stability are UNPROVEN
  from a single interval — not a claimed stable always-on guarantee.
  Raw transcripts: evidence-dials.txt.
