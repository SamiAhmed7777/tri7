# Judge brief: tri7 lane-gui-docs — parity backlog truthing + GUI options audit

## What this artifact is
Two DOCUMENTATION files on tri7 (repo /root/tri7), produced by two delegate
agents (MiniMax Code M3 = backlog truthing; GLM subagent = options audit),
adopted by the parent session. NO code changes (verified: worktree git status
shows only these two doc paths; zero .rs modifications).

1. docs/LEGACY-PARITY-BACKLOG.md — checkbox states updated to match live
   reality: [x] on verbs live-probed on the fleet node 2026-11-09 by the
   parent (keypoolrefill, listaddressgroupings, repairwallet/resendtx,
   getcheckpoint, reservebalance, smsgoptions/scanchain/scanbuckets, toggles,
   backupwallet, importprivkey, gettransaction, addmultisigaddress,
   addredeemscript), plus item-15 GUI rows (tray, start-on-login, reserve)
   and item-16 installer refresh (7.0.2). Items currently being built by
   in-flight lanes are annotated "(lane in progress 2026-11-09)" and left
   unchecked. Deliberate non-goals untouched.
2. docs/GUI-OPTIONS-AUDIT.md — NEW: 17-row audit of every legacy Qt Options
   row (optionsdialog.cpp 507 lines + optionsdialog.ui) vs tri7 src/gui/*.rs,
   each status claim pinned to file:line; ends with a prioritized 5-item
   next-GUI-work queue. Parent spot-checked the "Display addresses" claim
   (options field persists at app_state.rs:67 but the tx-table columns at
   app.rs:3827-3832 render unconditionally — claim verified TRUE).

## Verification
- Diff confined to the two doc files (backlog.diff attached; audit is a new
  file, attached in full).
- No code, no build, no tests affected — docs-only commit.

## ROUND 2 — judge blockers addressed
1. **Audit summary arithmetic** — Score line corrected to the table's real
   classification: 6 shipped / 5 deliberately dropped / 2 missing honest
   stubs / 2 partial / 1 disabled-candidate (detach) / 1 shipped-differently
   (verified by mechanical grep of the status column: 6/5/2/2/1/1 = 17 rows).
2. **Stale prose on flipped backlog rows** — every newly-checked line now
   carries accurate shipped-state prose (smsg toggles, start-on-login,
   staking reserve, tray icon), and a **checkbox-semantics legend** was added
   under Evidence base: "[x] = exists AND answered a live probe on the fleet,
   2026-11-09; prose notes describe scope, never unavailability". The
   addmultisigaddress P2SH note is a SCOPE note (spending remains out of the
   signing stack; creation+storage shipped in item 12) — kept, now covered by
   the legend.
3. **Reproducible evidence** — probes.txt in this bundle: fresh raw responses
   from the DNS2 tri7 node for every verb the backlog newly checks (shipped
   verbs answer shaped results; the -8/-1 validation errors for write verbs
   prove the handlers are routed), plus getstakinginfo.reservebalance that
   the GUI row loads. Installer-refresh claim is pinned to commit 9723e33
   (item 16, on master — judge can read /root/tri7 git log).

## ROUND 3 — judge blockers addressed
1. **Untruncated probes** — probes.txt v2: full responses (2000-char cap) for
   listaddressgroupings + getcheckpoint; plus a LIVE smsg
   enable→disable→options→re-enable→options roundtrip with raw outputs
   (disable answers -1 "Secure messaging is disabled.", re-enable answers
   "Enabled secure messaging.", options read back both ways).
2. **smsg toggles / autostart / tray evidence** — smsg toggles: bundled
   roundtrip above. Autostart: autostart-check-and-scope.txt carries a FRESH
   live run of the shipped GUI's headless check on sami-pc:
   `tri7_wallet_gui.exe --autostart-check` → "autostart: supported=true /
   autostart roundtrip: PASS (registered, read back, removed cleanly)".
   Tray: claim narrowed — the backlog row now cites its graded evidence
   source (item-15 gate bundle .gate/gui-polish-2026-11-09/) instead of
   implying a fresh UI session.
3. **Installer row reconciled with the legend** — UNCHECKED. It now reads:
   code + CI artifact landed (9723e33), live silent-install exercise
   PENDING because the v7.0.2 release-publish run failed
   (`Distribute Release (tri7)` run 42714180707) — post-push work is no
   longer described as completed evidence.
4. **Worktree-scope claim verifiable** — autostart-check-and-scope.txt
   captures `git status --porcelain` for BOTH the lane-gui worktree (only
   the two doc paths + task/log files) and /root/tri7 at bundle time.

## ROUND 4 — judge blockers addressed
1. smsgscanbuckets raw response ADDED to probes.txt ("found":0, "Scan Buckets
   Completed.").
2. Legend filename fixed: cites autostart-check-and-scope.txt (the actual
   bundled file name).

## ROUND 5 — judge blockers addressed (citation defects inside the audit)
1. `docs/tor-networking.md` (nonexistent) replaced with in-repo authoritative
   sources: src/onion.rs + the daemon's --socks/--onion-only dialing, with the
   full policy located in the triangles-modernization skill reference
   (outside the repo, stated as such).
2. Options persistence citation corrected: state.rs:215 only builds the path;
   the real write is `persist_options` (app_state.rs:3106) called from
   `save_options_action` (app_state.rs:2797) with `options_path()`
   (state.rs:213-215) — the fee row now cites that chain.

## ROUND 6 — judge blocker addressed
UPnP row rewritten with the capability/policy distinction the judge demanded:
the binary RETAINS a Clearnet dial path (follow_tip.rs PeerTarget::parse
classifies plain IPs as Clearnet; --listen-bind/--listen-port exist), while
fleet DEPLOYMENT POLICY is Tor-only + I2P via conf peers + --socks/--onion
launch lines (standing operator directive). The GUI-row disposition is
unchanged (no proxy editing in the wallet UI; transport panel instead).

## ROUND 7 — judge blocker addressed (fleet-policy claim RETIRED)
The judge's live-code finding was correct and went further than the doc:
DNS2's LIVE conf (bundled capture live-dns2-transport.txt: unit + cmdline +
conf peer lines + getpeerinfo) peers over three direct tailscale IPs PLUS one
onion PLUS one i2p b32 — the "fleet is Tor-only" claim was FALSE for the
current deployment and has been RETIRED from the audit. The UPnP row now
describes the observed deployment (direct-IP dial path retained in code;
tailscale + onion + i2p peers in live conf; phase6 deploy template with
direct-IP --daemon peers) and explicitly makes NO fleet-wide transport claim;
UPnP's disposition is justified by NAT traversal being handled by explicit
peer config instead.

## ROUND 8 — judge blockers addressed
1. Residual "Tor-only by design" policy language on the SOCKS-proxy row
   rewritten to the observed per-node MIXED transport configuration
   (conf: three direct tailscale IPs + one onion + one i2p target).
2. live-dns2-transport.txt now carries the VERBATIM getpeerinfo output:
   count 3, by_transport {clearnet: 2, i2p: 0, tor: 1}, with each peer's
   endpoint/inbound/transport/since. Brief wording narrowed: addnode lines
   are CONFIGURED peer targets; the getpeerinfo block is the ACTIVE
   connection snapshot at capture time.

## ROUND 9 — judge blockers addressed
1. Brief numbers aligned to the bundled capture: count 4 = clearnet 3 + tor 1
   (the round-8 prose quoted an earlier capture's count 3 — peer set changed
   between captures; the bundle now contains ONE self-consistent session).
2. live-dns2-transport.txt now includes the RAW unmodified getpeerinfo
   JSON-RPC response, followed by a parsed summary OF THAT SAME response —
   "verbatim" is now literally true.
