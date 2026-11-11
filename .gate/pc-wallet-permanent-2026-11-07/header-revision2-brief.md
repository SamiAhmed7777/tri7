# Header artwork updated to the operator's revised white PNG (same path)

The operator replaced "_TRI logo w name new white.png" on the PC with a
revised version and directed again: use this for the header. The asset
src/gui/assets/brand/brand_header_white.png is refreshed byte-for-byte from
the new file (sha c2962a15..., 1199x252 RGBA — identical dimensions to the
previous revision; zero code change, the texture key already points here).
Also rides in this build: the five-row balance card + fixup (6dc22d5).

cargo test --release --lib: 491 passed, 0 failed.
