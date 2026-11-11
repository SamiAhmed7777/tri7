# Header wordmark swapped to the operator's new white artwork

Operator supplied "_TRI logo w name new white.png" (1199x252 RGBA, from the
PC E:\Downloads) and directed: use this for the TRI header.

- New asset: src/gui/assets/brand/brand_header_white.png (the operator's
  file, byte-for-byte).
- The texture map entry "brand/header" now points at the white artwork
  (was brand_header_new@2x.png). Same 4.758 aspect as the previous asset, so
  the 56px header height renders at the same width — only the art changes.
- All three render sites (header bar, About drawer, installer page) pick it
  up via the same key — no layout change.

cargo test --release --lib: 491 passed, 0 failed. Evidence: the diff + the
new asset file (provenance: operator-supplied, this session).
