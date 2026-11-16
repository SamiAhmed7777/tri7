# §17 PC onion endpoint triage + the PC's start-only watchers (item 22, 2026-11-11)

Proposed append to SKILL.md as a new numbered section. Evidence for every
fleet claim: /root/tri7/.gate/pc-endpoint-2026-11-11/ (evidence-timeline.md,
evidence-dials.txt, torinfo-evidence.log, torinfo-final.log, hscheck.ps1,
evidence-dns2-journal.txt, evidence-dns2-status.txt). General tor guidance
below is marked [tor-general] and is secondary to the tor project's own docs.

## 17. Onion endpoint triage: three layers, scoped conclusions, and the PC's start-only watchers (item 22)

- An "onion is dead" report conflates three independent layers. On sami-pc
  (observed 2026-11-11, host-labelled evidence in the bundle): (1) FETCHABILITY
  — dial the onion from a healthy remote tor (DNS2's own worked); a dial that stayed pending until the observer's timeout (35-45 s) was
  measured = the onion connection did not COMPLETE during the observation
  window (the stall may sit at descriptor lookup, introduction, rendezvous,
  or the service response — localize to a stage only with client-side tor
  logs, a definitive SOCKS error code, or service-side info logs; do not
  jump to "descriptor unfetchable" from a timeout alone); a SOCKS5 grant seconds in =
  fetched + rendezvous-joined, proving reachability AT THAT TIMESTAMP ONLY.
  (2) SERVICE LIVENESS — the service's own tor heartbeats log "received N v3
  INTRODUCE2 cells": no INTRODUCE2 was observed during the measured heartbeat window (~40h on
  the PC) — i.e., no introduction was even attempted successfully; INTRODUCE2
  receipt is upstream of rendezvous completion and backend acceptance, so
  this bounds "no dial ever got through the introduction" and nothing more.
  It does NOT by itself localize whether the cause was upload failure,
  fetch-side unreachability, or simply zero dial attempts — distinguish those
  with an info-level upload log before blaming the publisher. (3) BACKEND BIND — the listener must hold the exact IP:port
  the torrc HiddenServicePort targets; on this fleet the i2pd daemon (also
  SYSTEM) successfully dialing the same IP:port is a cheap cross-check that
  the local dial path works. ControlPort GETINFO circuit-status is the fastest
  liveness probe: HS_SERVICE_INTRO circuits in HSSI_ESTABLISHED in the
  snapshot = intros were up AT SNAPSHOT TIME; their absence in ONE snapshot is
  consistent with transient circuit churn and must be re-sampled before
  concluding "never registered".
- Fleet-observed signature (PC, one instance, 2026-11-11 — do NOT generalize
  to all of tor): while the tor instance was sick, its own log showed
  `log_cant_upload_desc: can't upload ... Next upload time is T, it is now N`
  with T≈N+52min — the service's upload scheduler deferred publication (a dial failed at
  10:11Z — CORRELATION with the 09:56 restart, cause not established; the
  10:16 INTRODUCE_ACK line in the scrubbed log is not attributable to this
  service), and recovery is evidenced by the RAW 10:37-10:38Z pair (SOCKS
  grant + the production daemon's backend-accept journal line); the
  09:41-09:48Z early dials are RECONSTRUCTED summaries, so the recovery
  conclusion rests on the raw pair, not on them. [tor-general] descriptor overlap is DESIGNED to keep healthy
  services continuously dialable; long unfetchable windows are a DEFECT
  signature (upload scheduler/guards/directory state), not normal behavior —
  treat repeated dial failures across a re-upload cycle as "publisher
  unhealthy", not "tor being tor".
- Recovery lever used on the PC (measured to work there; a fleet-specific
  ESCALATION after preserving evidence, not a routine first-line tor
  recovery step): restart the tor instance and MOVE the DataDirectory aside
  (rename, reversible) so tor rebuilds guards/directory state, then verify recovery via the upload 200
  line (`handle_response_upload_hsdesc: ... status 200 ("HS descriptor
  stored successfully.")`) captured at info level. CONSEQUENCES to accept
  explicitly: a fresh DataDirectory discards the guard selection and cached
  directory state (bootstrap takes ~1-2 min; guard quality re-rolls); when
  the same tor instance also performs CLIENT dials, its client-side
  descriptor caches are lost too and early dials fail until it re-fetches. NEVER touch HiddenServiceDir/hs —
  the onion address is load-bearing in fleet configs (the address did not
  change). Info-level tor logs contain sensitive data: capture to a dedicated
  file, revert to notice via SETCONF afterwards, and treat the capture as
  evidence (hashed) — do not leave info logging on.
- ControlPort on the PC was configured with CookieAuthentication 0 (no
  auth — the probes in the bundle authenticated with a bare AUTHENTICATE,
  proving the no-auth state; the setting lives in C:\Users\hello\tri7\tor\torrc
  on the host, which also warns about it on every start). That is an existing risk to FIX (set
  CookieAuthentication 1 or a strong control password; keep it loopback-only)
  — not a tool to rely on. The probes used this tick (hscheck.ps1 /
  setinfolog.ps1 / setnolog.ps1 in the bundle) worked only because of that
  misconfiguration; do not replicate the no-auth pattern elsewhere.
- sami-pc restart churn has TWO named start-only watchers (both observed on
  the host, evidence in the bundle + C:\Users\hello\watchdog.log):
  `tri7d-watchdog` (v5 task) and `tri7watch` (v6 task → watchdog6.ps1: "no
  19114 listener → kill stray state2 follow_tip + Start-ScheduledTask
  tri7d2 + append watchdog.log"). watchdog6.ps1 fired every 5-20 min through
  the 07:20-08:56Z window (the exact chronic churn); against a DISABLED
  tri7d2 its Start-ScheduledTask fails silently (v6 never checks the enabled
  state) — a start-only watcher pointed at a disabled target manufactures
  boot churn forever. Disable BOTH watchers before any PC state-dir repair
  (generalizes the item-12 tri7d-watchdog rule; watchdog.log timestamps
  against daemon boot lines identify which watcher fired). The SEPARATE
  silent hard exit of follow_tip (rc=1, no error line, healthy one minute
  and gone the next; reproduced on demand 08:42-08:45Z; WER shows only
  LiveKernelEvent records) is a DIFFERENT, still-unsolved layer — interactive
  AVG/WER diagnosis owed (operator's window).
