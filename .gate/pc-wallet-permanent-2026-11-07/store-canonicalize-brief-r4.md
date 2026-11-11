# store_canonicalize — repair a block store whose height map has a hole

Same artifact as the previous round (which graded B), with the judge's three
polish items applied:

  1. the source identity (length + mtime) is now re-checked at EVERY
     protected boundary: at the start of the copy, after the copy, after the
     final output verification, and before a successful --verify-only
     return;
  2. the documentation now names the exact protected intervals and states
     plainly what is NOT covered: a same-size in-place mutation of an
     EXCLUDED frame, and a change to the source after the final check but
     before the operator swaps the file in;
  3. a new regression test covers an append during verify-only.

Evidence files:
  store-canonicalize.diff            the exact diff under review
  store-canonicalize-build-test.log  cargo build + cargo test output from
                                     the tree that holds that diff
  store-canonicalize-live-verify.log the live DNS2 verification with full
                                     provenance (command, timestamps, the
                                     sha256 of the binary AND of the
                                     reviewed source it was built from, the
                                     input files' metadata and sha256, the
                                     tool's stdout, and the exit status)

The defect, the fix and every safety property are unchanged: the canonical
chain is discovered from the file's own prev links, every frame is
re-verified link by link, the genesis anchor is enforced in every case, a
missing frame / broken link / truncated frame / non-contiguous window /
incomplete history / ambiguous duplicate header hash ABORTS the run rather
than dropping and re-anchoring, the source is never modified, and the
output is created new, fsynced and compared byte for byte with the source
before exit.

Tests: 19 focused tests in the same file, plus the 507-test lib suite, all
green.
