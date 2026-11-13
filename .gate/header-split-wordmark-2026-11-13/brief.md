# Brief — header wordmark: split lockup, bigger words (Sami 2026-11-13)

## Purpose
Sami directives, in order: "make the word mark bigger" -> correction "just
the word itself bigger and leave the icon the same size" -> "it should be
relatively easy to split them right" -> picked 30 px words + taller 88 px
header (clarify answer verbatim: "30px words + taller 88px header (bottom
row of the picture you sent)").

## What changed
1. The 1199x252 combined lockup (brand_header_white.png) was split at its
   natural gaps (columns with no alpha>32 pixels: x=196..201 and 789..812)
   into three content-tight crops, embedded as new assets:
   - brand_lockup_icon.png 186x162 (the triangle)
   - brand_word_cryptographic.png 587x59 ("Cryptographic")
   - brand_word_triangles.png 378x59 ("Triangles")
2. The header composes the three (icon -> space -> words), replacing the
   single scaled lockup image:
   - icon UNCHANGED at 36 px content height (the old 56 px lockup showed the
     162 px triangle at 162x56/252 = 36.0 — identical size, now crisp at
     native crop resolution)
   - each word 30 px tall (was 59x56/252 = 13.1 px) — Sami's pick, previewed
     and approved
   - header height 64 -> 88 px to fit the bigger words
   - aspect ratios are the crops' own; no font substitution, the artwork is
     the operator's PNG lettering (brand rule: never a plain-font label)
3. Content test header_word_is_bigger_and_icon_size_is_unchanged pins: the
   three assets in the map, the header rendering them (and never falling
   back to the combined lockup), exact_height(88.0), the icon's 36 px ratio
   line, the words' 30 px ratio lines, and the three crop files' exact PNG
   pixel dimensions on disk.

## Definitions
- "full lib suite" -> cd /root/tri7 && cargo test --lib
- "GUI featureful check" -> CARGO_TARGET_DIR=/root/tri7/target-gui-check
  cargo check --bin tri7_wallet_gui --features gui
- "windows release build" -> CARGO_TARGET_DIR=/root/tri7/target-win cargo
  build --release --target x86_64-pc-windows-gnu --features gui --bin
  tri7_wallet_gui
- "the approved look" -> wordmark-final-30-88.png (rendered from the split
  assets at exactly the shipped sizes/positions)

## Evidence (bundle files)
- header-split.diff — git diff of src/gui/app.rs + src/gui/tests.rs vs HEAD
  (includes the three binary asset adds as file entries)
- tests-header-split.txt — full raw lib suite (762 passed; 0 failed; 1
  ignored) + GUI featureful check Finished, both streams
- brand_lockup_icon.png / brand_word_cryptographic.png /
  brand_word_triangles.png — the three embedded crops (186x162 / 587x59 /
  378x59)
- wordmark-final-30-88.png — the approved look rendered from those assets
- wordmark-build-subsystem.txt — windows release build log tail + PE
  subsystem parse (2 = GUI) + sha256 of the artifact
- mockup-sheet.png — the size-option sheet Sami chose from (13/16/18/20/22/
  24/30 previews)

## Files
brief.md
header-split.diff
tests-header-split.txt
brand_lockup_icon.png
brand_word_cryptographic.png
brand_word_triangles.png
wordmark-final-30-88.png
wordmark-build-subsystem.txt
mockup-sheet.png
