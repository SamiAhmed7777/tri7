# Round 7 — last round's blockers, addressed

B1 "Remove the duplicate `let mut script` declaration and rerun the actual
release library test suite."
  -> There is no duplicate: the report came from a diff ARTEFACT (the hunk
     shows the old line being REPLACED by the corrected one). The patched
     tree has exactly ONE `let mut script` and compiles. The evidence now
     carries the VERBATIM test output from this exact patched tree (the
     `cargo test --release --lib` tail, 507 passed / 0 failed), and the
     base revision plus the exact commands to apply the diff and reproduce
     it.

B2 "Provide reproducible test output from the exact patched tree; the current
507-pass claim cannot describe this uncompilable diff."
  -> The evidence file now contains the literal test output, the base
     revision (ee5534dfdca885851d265c907ae4db95bbc82ccb), the `git apply` recipe, and the probe's full source.

Polish applied: `addrindex::wallet_view` now uses rusqlite's RAII
`Connection::transaction()` as well (not a hand-rolled BEGIN DEFERRED), and
a COMMIT failure is propagated instead of ignored.

Test output (verbatim, this tree): 507 passed; 0 failed; 1 ignored.
