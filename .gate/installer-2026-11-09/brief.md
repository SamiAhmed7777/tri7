# Judge brief — workqueue item 16 INSTALLER (tri7, 2026-11-09)

Artifact: Windows installer refresh + auto-updater exercise. Scope: the
installer payload/metadata fix + version refresh + test-fixture alignment +
release-body digest-pin shape fix. The LIVE exercise (release publish, DNS2
engine probe, sami-pc silent install) runs post-push at tick close; the
workqueue PROGRESS line carries that evidence.

## Changes (diff.patch)
1. Cargo.toml/Cargo.lock/BUILDING.md: version 7.0.1 -> 7.0.2. Rationale: the
   item is "installer refresh"; a NEW release version is what the in-app
   updater (src/updater.rs) can resolve as strictly newer. CI names every
   artifact/asset from this single source.
2. deploy/windows/tri7-wallet.nsi:
   - ships txhistory.exe (item-14 remainder; File line + uninstall Delete +
     header comment). Decision was explicitly deferred to item 16; shipped.
   - EstimatedSize: was the static DWORD 25 (KB) vs ~18 MB real payload
     (item-14 remainder, cosmetic). Now FileFunc ${GetSize} "$INSTDIR"
     "/S=0K" measured at install time, written as the KB DWORD ARP expects.
     Measured AFTER the File lines so it counts the real payload + node dir.
   - !include "FileFunc.nsh" added.
3. .github/workflows/ci.yml: the installer step's Copy-Item gained
   target\release\txhistory.exe (makensis reads deploy\windows\; without the
   copy the new File line would fail the windows job).
4. src/updater.rs + src/gui/tests.rs: fixtures that modeled "a newer
   release" used 7.0.2 == the new current version, breaking the
   newer-than-current premise -> fixtures bumped to 7.0.3 (sample_release
   JSON + dependent asserts + banner test + skip/persist asserts + the
   further-new case to 7.0.4). Semver-mechanics tests (pure comparisons)
   deliberately untouched. Local: makensis syntax check green; 530 lib + 64
   follow_tip tests green.

## Known deviations / honesty notes
- The updater's digest pinning has been INERT since v7.0.1: its release body
  quoted "SHA256 (win64-setup.exe): <hex>" — the extractor needs the FULL
  asset name on the line (extract_digest_from_body), so pinned=None and the
  documented size-floor+PE-sanity fallback applied. Not fixed in code (the
  code is correct; tolerant fallbacks exist) — fixed at the SOURCE: v7.0.2's
  release body will quote the exact CI SHA256SUMS rows (shape A). Noted in
  HANDOFF.
- txhistory.exe ships in the installer only; it is NOT a standalone release
  asset (the updater never resolves it; 4 documented flavors unchanged).
- EstimatedSize measures $INSTDIR at install time — on an UPGRADE over an
  existing folder it also counts pre-existing content (wallet.dat, node
  state). This is ARP- EstimatedSize semantics ("approximate installed
  size"); documented in the nsi comment.

## Acceptance checks claimed
- nsi compiles (makensis local green; CI windows job must stay green).
- All four exes present in the built installer payload (verified post-push
  from the CI artifact by 7z listing + hash compare).
- Local test battery green (530 lib incl. all updater tests, 64 follow_tip).
- Post-push live: updater engine resolves+downloads+verifies v7.0.2 on DNS2;
  silent /S /D= install on sami-pc shows real EstimatedSize, DisplayVersion
  7.0.2, bin\txhistory.exe present, hashes == CI artifacts, GUI relaunches.

## Round 2: polish taken from round 1 (grade B, urn:ump:fuzfea46ni3nxebtojicuv7ag2y5qalo5e56dptafl3rutmi6huq)
- TAKEN: fallback `!define VERSION` refreshed 7.0.0 → 7.0.2 (CI -DVERSION
  remains the authoritative override).
- TAKEN: ${GetSize} EstimatedSize moved after WriteUninstaller so the
  uninstaller is counted; the ARP-semantics over-count note is now in the
  nsi comment itself.
- RECORDED (not taken): measuring only bin\ + fresh node dirs on upgrade —
  declined; folder-approximation is what ARP EstimatedSize means, and the
  installer docs state the folder IS the wallet folder.
- Bytes changed since round 1: tri7-wallet.nsi only (the two moves above);
  syntax check re-run green.
