# store_canonicalize — repair a block store whose height map has a hole

This is the SAME artifact that graded B in the previous round, with the
judge's three polish items applied:
  1. the source identity (length + mtime) is now re-checked AFTER the copy
     as well as before it, via a shared assert_source_unchanged helper;
  2. the append test now exercises that helper and asserts the repair
     rejects the append, instead of only asserting that the metadata moved;
  3. the output is fsync'ed (sync_all) before the tool reports success, so
     a later power loss cannot leave a half-written store.

Evidence files:
  store-canonicalize.diff            the exact diff under review
  store-canonicalize-build-test.log  cargo build + cargo test output from
                                     the tree that holds that diff
  store-canonicalize-live-verify.log the live DNS2 verification with full
                                     provenance (command, timestamps, binary
                                     and source hashes, input metadata and
                                     sha256, stdout, exit status)

The defect, the fix and every safety property are described in the previous
round's brief and are unchanged; the diff now carries the three polish
items above. The tool's own doc header states the source-change detection
scope precisely: the file's length and mtime are checked for appends and
replacements, every copied frame carries a sha256d digest so a mutation of
a copied frame is detected, and a same-size in-place mutation of an
EXCLUDED frame is not detected because excluded frames are never copied.

Tests: 18 focused tests in the same file, plus the 507-test lib suite, all
green.
