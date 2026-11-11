# Workqueue item 14 CI-ARTIFACT — confirmation bundle

## Task (from TRI7-WORKQUEUE.md)
"add tri7.exe (the CLI) to the CI windows artifact list and the installer
payload." NOTE 2026-11-09 (item-12 tick): tri7.exe IS already shipped by CI
run 42637049872 alongside follow_tip/tri7_wallet_gui/txhistory — the
remaining bar was the INSTALLER payload + confirmation.

## What was already landed (prior ticks, both on origin/master)
Commit 71bfc9a (2026-11-08, graded B
urn:ump:2f1bk8rq2m3k4qvn7x0p9rj5c1sd8vg6hqw4nt2mzl7rk0dp) wired tri7.exe
into BOTH distribution surfaces:
- .github/workflows/ci.yml: the tri7-windows upload-artifact list carries
  target/release/tri7.exe, and the "Build Windows installer (NSIS)" step
  Copy-Items tri7.exe into deploy/windows/ before makensis.
- deploy/windows/tri7-wallet.nsi: `File "${EXEDIR}\tri7.exe"` installs it
  to bin/tri7.exe; the Uninstall section deletes it.

## What THIS tick did (the remaining bar: confirmation on a green run)
No code change. Evidence chain, CI run 42649739291 (commit 9cb337e — the
tip of master before this docs commit), all 4 jobs success
(linux-tests, linux-gui-tarball, windows-build, macos-build):

1. ARTIFACT LIST: downloaded the tri7-windows artifact — contains
   follow_tip.exe, tri7_wallet_gui.exe, tri7.exe (4,147,200 B),
   txhistory.exe. tri7.exe sha256
   1df8713c8f12d317aba761c162fcde121e9833d5b38574bd41dd1247983ddd6d.
2. INSTALLER PAYLOAD: downloaded the
   CryptographicTriangles-7.0.1-win64-setup.exe artifact (7,384,576 B) and
   7z-listed it: Type = Nsis, NSIS-3 Unicode, LZMA solid. Payload =
   bin/tri7_wallet_gui.exe (12,121,088 B) + bin/follow_tip.exe (6,685,696 B)
   + bin/tri7.exe — exactly the nsi File lines, tri7.exe INCLUDED.
3. BYTE PARITY: extracted bin/tri7.exe from the setup.exe; cmp clean —
   BYTE-IDENTICAL to the tri7-windows artifact's tri7.exe (same sha256).
   The installer ships the same CLI binary CI builds.
4. Payload build lineage: the run's nsi == master's nsi (no drift; the
   tri7.exe File line is in the commit the run built from).

Version metadata resolution works end to end: the artifact names carry the
Cargo.toml version (7.0.1), matching the updater's
CryptographicTriangles-<ver>-win64-setup.exe convention (src/updater.rs).

## Honest remainders (recorded in HANDOFF, deferred to item 16)
- Installer payload is 3 exes; txhistory.exe is artifact-only (nsi has no
  File line). Whether to ship it is item 16's call during the installer
  refresh.
- The nsi's uninstall registry EstimatedSize is a static 25 (KB) vs ~18 MB
  real payload — cosmetic, fix opportunistically in item 16.
- A live silent-install exercise (/S + /D=) on sami-pc is item 16's bar
  (installer refresh + auto-updater), NOT this item's.

## Artifact under judgment
The HANDOFF.md section documenting this confirmation (handoff.diff) — this
is a verification tick, same shape as item 3 (tor-publish, HANDOFF-only
diff, graded A) and item 13 (disk-watch, ops artifact, graded A). Judge the
docs: are the claims supported by the evidence files, is the provenance
honest (prior commit credited, no code change claimed), are the
remainders correctly scoped?
