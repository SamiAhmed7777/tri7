#!/usr/bin/env bash
# verify-offline.sh — runs on ONLY the files in this bundle (no repo, no
# network, no Rust toolchain). It proves the EXACT reconstruction identity:
#   base-node_embedded-f08d29d.rs  + adoption-body-stall.diff
#      == patched-node_embedded-r6.rs   (byte-for-byte, sha256)
set -euo pipefail
cd "$(dirname "$0")"
echo "== bundle files =="
ls -la
echo "== sha256 of every artifact =="
sha256sum base-node_embedded-f08d29d.rs adoption-body-stall.diff patched-node_embedded-r6.rs 2>/dev/null || sha256sum *.rs adoption-body-stall.diff
echo "== exact reconstruction: patch(1) the base with the graded diff =="
rm -f reconstructed.rs
cp base-node_embedded-f08d29d.rs reconstructed.rs
patch --quiet -p1 reconstructed.rs < adoption-body-stall.diff
echo "patch rc=0 (all hunks applied cleanly)"
echo "== byte-for-byte comparison =="
sha256sum reconstructed.rs patched-node_embedded-r6.rs
cmp reconstructed.rs patched-node_embedded-r6.rs
echo "RECONSTRUCTION EXACT: base + diff == patched source (byte-for-byte)"
