# store_canonicalize — repair a block store whose height map has a hole

This is the SAME artifact that graded B two rounds ago, with the judge's
blocking items from the last round now closed:

  1. THE LIVE VERIFICATION NOW USES A BINARY BUILT FROM THE EXACT REVIEWED
     SOURCE. store-canonicalize-live-verify.log records, in one run:
       - the exact command,
       - the UTC and local timestamps,
       - the sha256 of the BINARY that ran,
       - the sha256 of the REVIEWED SOURCE it was built from,
       - the input files' metadata and sha256,
       - the tool's stdout, and
       - the exit status (0).
     The source hash in that log and the source hash in
     store-canonicalize-build-test.log are the same file, and the diff in
     this bundle is that file's diff.

  2. THE APPEND REGRESSION TEST NOW EXERCISES THE REAL COPY PATH. The
     identity check (the source's length and mtime) moved INTO
     write_canonical, so the copy itself rejects an append. The test
     appends to the store between the scan and the copy and asserts that
     write_canonical returns the refusal — it no longer calls the helper
     directly. An unchanged source still passes the same check.

  3. Polish: the duplicated flag comment is gone, and the fsync comment now
     says what syncing does (the file's CONTENTS become durable) and notes
     that the newly created PATHNAME also needs the parent directory synced
     for crash durability of the name itself.

Evidence files:
  store-canonicalize.diff            the exact diff under review
  store-canonicalize-build-test.log  cargo build + cargo test output from
                                     the tree that holds that diff
  store-canonicalize-live-verify.log the live DNS2 verification with full
                                     provenance

The defect, the fix and every safety property are unchanged from the
previous brief: the canonical chain is discovered from the file's own prev
links, every frame is re-verified link by link, the genesis anchor is
enforced in every case, a missing frame / broken link / truncated frame /
non-contiguous window / incomplete history / ambiguous duplicate header hash
ABORTS the run rather than dropping and re-anchoring, the source is never
modified, and the output is created new, fsynced and compared byte for byte
with the source before exit.

Tests: 18 focused tests in the same file, plus the 507-test lib suite, all
green.
