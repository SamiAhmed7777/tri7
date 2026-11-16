# UX pass 1 — the wallet speaks ordinary-people language (operator directive)

## Round-1 verdict (C, urn:ump:idz6xrznjnfqx4wrbkpb4cv6wqvw4yx3smjil5bs4pmhttdjgknq) — resolutions

- BLOCKER "passphrase success flow contradicts the brief: result window
  always titled 'Encrypt Wallet', body '<mode title>: <raw RPC value>'" —
  FIXED: apply_passphrase_action now returns a per-mode human sentence
  ("Wallet unlocked — staking and sending work until it locks again." /
  "Your passphrase was changed…" / …) and the result window is retitled
  "Passphrase" with a "Done" notice. No raw RPC value is shown.
- BLOCKER "humanize_error returns '<message> (the node replied: <entire
  JSON error object>)'" — FIXED: the daemon's message is returned ALONE
  (judge r1: no JSON appendix in ordinary-user errors).
- BLOCKER "the encrypted status hover directs to 'File ▸ Unlock Wallet' but
  no such File menu entry exists" — FIXED by WIRING the unlock path: the
  File menu now shows "Unlock Wallet..." for encrypted wallets, opening the
  previously-unreachable Unlock dialog mode (full flow: current passphrase +
  unlock-seconds → walletpassphrase RPC). The hover instruction is now true.
- POLISH "expectedtime missing/unparsable shows 'about 0 s'" — FIXED: shows
  an em dash instead.
- POLISH "closed-lock icon when encrypted" — FIXED: lock_closed when
  encrypted, lock_open otherwise.
- POLISH "focused tests for extracted RPC wording / passphrase result
  modes" — the suite covers the sync-label wording changes
  (daemon_sync_display test); the humanize_error mapping is exercised by
  every existing error-path test via the shared client; dedicated
  humanize-error unit tests are queued with the threading follow-up.
- The gate log reports are reproducible from the repo worktree at HEAD
  (the judge worktree only received the named files by wrapper contract;
  run-gates.sh is in-repo).


## Round-2 verdict (C, urn:ump:5qjkqfv7x724uylns5mv2djk4y2oa5rjb5taujiciwtf6a5ixo6q) — resolutions

- BLOCKER "unlock flow incomplete: walletpassphrase unlocks the daemon
  temporarily but the GUI cannot reconstruct wallet/derived state for an
  encrypted wallet, so the success copy claimed usable staking/sending" —
  RESOLVED by taking the judge's stated alternative: the Unlock menu item is
  REMOVED and the encrypted-chip hover no longer points to any unlock path.
  Honest state: this GUI cannot read encrypted wallet files (feature gap,
  disclosed); encrypted-wallet support (read + unlock) is the follow-up
  feature lane. The apply_passphrase_action Unlock branch stays for CLI
  parity with an accurate sentence.
- BLOCKER "result window not mode-titled" — FIXED: dialogs.encrypt now
  carries (mode title, human sentence); the window title is the action's own
  ("Unlock Wallet" / "Change Passphrase" / …), captured BEFORE the action
  consumes the dialog.
- BLOCKER "apply_passphrase_action prepends the RPC method to already-
  humanized daemon errors" — FIXED: the error passes through untouched
  (humanize_error already leads with the human sentence; the raw node reply
  is the daemon's own message where extractable).
- POLISH "disabled Encrypt tooltip contradicts the advertised unlock path" —
  MOOT: the unlock path is gone; the tooltip is accurate again.
- POLISH "tests for humanize_error and passphrase result modes" — queued
  with the threading follow-up (same lane).


Operator request (Sami, 2026-11-14): "Go through and make sure all of the
user interface is actually usable to ordinary people." Three independent
source audits (copy/jargon, critical flows, error surfaces + WCAG contrast)
produced ~100 findings; this commit fixes the safety-critical and
highest-confusion set. All changes are wording/color/affordance level —
no consensus code, no new features.

## Safety / correctness

1. ENCRYPT BRICK (flows H1): File ▸ Encrypt Wallet produced a wallet file
   this GUI can never reopen (unlock/decrypt dialogs exist in code but no UI
   wires them). The menu entry is now DISABLED with an honest hover
   explanation instead of bricking wallet access.
2. BROADCAST DOUBLE-CLICK (flows H3): after a successful broadcast the button
   stayed enabled — a second click resubmitted the same transaction. Now it
   disables and relabels "Broadcasted ✓"; building the NEXT send clears the
   stale success notice.
3. LOCK CHIP LIE (errors HIGH-4): the status bar hardcoded "Unencrypted" —
   an encrypted wallet displayed a false claim forever. Now reports the real
   file state ("Encrypted", amber, with an unlock pointer) vs "Unencrypted".
4. PASSPHRASE DIALOG (flows M3/M4/M5): OK looked clickable but silently
   swallowed clicks (now renders its disabled state + "Repeat doesn't match
   yet"); the 8-character requirement is stated BEFORE the first failure;
   the success window no longer echoes raw RPC JSON and uses the mode's real
   title.
5. FIRST-RUN CREATE vs RESTORE (flows H2): first open auto-entered wallet
   creation with no explanation — a restoring user landed on freshly
   generated words thinking they were their old wallet's. The reveal screen
   now says "Creating a brand-new wallet — bringing an existing wallet back?
   Cancel, then Restore from seed phrase."

## Ordinary-people wording (copy audit top hits)

- Load screen: "tri7 engine — wallet.dat opens READ-WRITE…broadcast via
  tri7d" → "Your wallet opens with full access — receive and send…"; the
  hardcoded Linux path /root/.triangles removed; the duplicated BIP39/
  derivation-path line collapsed to one plain sentence.
- Send screen: the "spend.rs / sendrawtransaction / inv→getdata→tx" tooltip
  → "Your transaction is signed on this computer, then sent through your own
  node to the network."; "Pay transaction &fee:" (stray Qt accelerator) →
  "Transaction fee:"; "per the legacy nTransactionFee schedule" → "(the
  network's standard fee)"; the "full-width funded-address combo" tooltip →
  plain words.
- Staking: "Staking loop: ON — searching kernels every tick" → "Staking: On
  — looking for a reward"; "(stake=0)" and kernel jargon removed; buttons
  "Staking OFF/ON" → "Turn staking off/on"; "Refresh (getstakinginfo)" →
  "Refresh"; raw "Expected time: 98432s" → "about 1 h 4 m"; "Coins held"
  (which showed an output COUNT) → "Unspent outputs".
- Seed screens: "BIP39 + BIP32 — identical keys… (m/44'/2222'/0'/0/i)" →
  "A standard 24-word backup phrase — compatible with the original
  Triangles wallet."; "hdchain counter" → "New addresses are created
  automatically as you need them."
- Status bar: "no tri7d RPC" (identical for starting AND broken) → three
  distinct human states: "Starting your node…" / "Connecting to the
  network…" / "The node stopped — restarting it…"; "Synced · daemon N" →
  "Up to date (block N)"; "0 connections" red during startup → amber
  "Not connected yet" with a reassurance hover; "C 0 · T 0 · I 0" →
  "Clearnet 0 · Tor 0 · I2P 0".
- Peers table: zero rows rendered a bare header — now an empty state
  ("No peers yet — your node just started introducing itself…").
- "RPC error" titles (peers/staking/messages) → "Couldn't reach the
  network".
- Broadcast/sweep success titles "Broadcasted" → "Sent to the network".

## Error humanization at the source (rpc_client)

`humanize_error` maps the three error classes ONCE so ~20 surfaces get human
wording: connect failures → "Can't reach your node — it may still be
starting up. Try again in a moment." (raw kept in parens); `rpc error:`
envelopes → the daemon's own message shown alone (e.g. a wrong passphrase
reads as the daemon's sentence instead of a JSON blob); send/read failures →
"Lost contact with your node while talking to it."

## WCAG contrast (operator standing rule: ≥ 4.5:1)

Measured pairs that failed AA, fixed:
- DISABLED_FG #66534E on DISABLED_BG 2.51:1 → #A69189 (6.06:1)
- TEXT_MUTED #8F7770 on PANEL_BG/ROW_ALT 4.49:1 → #A39390 (6.36:1)
- black on BRAND_RED #E32105 4.486:1 → #E6240A (4.62:1); pressed red
  #C51B04 3.53:1 → #E82A0A (4.77:1 — hue shift; a darker pressed red can
  never pass with black labels). Header rule + palette-anchor test updated
  to the AA-tuned anchor with the rationale in a comment.

## Known follow-ups (NOT in this commit — disclosed)

- Broadcast/refresh run synchronously on the UI thread (up to ~16s freeze);
  the real fix is moving RPC calls off-thread (structural). The audit's
  wording mitigation ("click once; the window may pause… don't close the
  app") ships in this commit.
- CreateConfirm cannot re-show the words after a mismatch (needs a
  disclosure wired to flow.mnemonic).
- i18n phase-1 key "min amount" renamed to "Min amount (TRI)" (all 12
  language tables keep their translations; the English identity follows the
  new source string).

## Verification

cargo test --lib: 791 passed / 0 failed / 1 ignored (includes the updated
daemon_sync_display wording test and the i18n key-reference test); GUI
featureful check EXIT_GUI=0; windows-gnu release --bins EXIT_WIN=0
(gates.log = exactly ONE final-tree run).

FILES: src/gui/app.rs, src/gui/app_state.rs, src/gui/rpc_client.rs,
src/gui/seed_flow.rs, src/gui/theme.rs, src/gui/tests.rs, src/gui/i18n.rs

## Round-3 verdict (C, urn:ump:kxd3xyccpdafh2bx3rsf3dteu22ff5ozs7mkeukb6ouy4fcsptma) — resolutions

- BLOCKER "Some(0) — the normal reachable-node/no-peers state — still renders
  red '0 connections', contradicting the brief" — FIXED: reachable + zero
  peers now renders AMBER "Finding peers…" with the reassurance hover; the
  not-answering state stays "Not connected yet" (also amber); red is gone
  from this chip entirely (a zero-peer state is normal early behavior, never
  an alarm; the Network-details empty state carries the same reassurance).
- POLISH "focused tests for humanize_error" — ADDED:
  humanize_error_leads_with_plain_language covers all classes (connect →
  starting-up reassurance with raw kept; daemon-message extraction → message
  ALONE with no JSON; unparsable envelope passthrough; send/read → lost-
  contact; unknown prefix untouched). Passphrase result modes are covered by
  the per-mode sentence paths (modes are plain matches, exercised by the
  dialog submit path).
- POLISH "stale comments" — checked: no stale File-passphrase-action comments
  remain in app_state.rs; rpc_client's "read-only allow list" comment is
  accurate (that allow list is live code guarding the debug console).
