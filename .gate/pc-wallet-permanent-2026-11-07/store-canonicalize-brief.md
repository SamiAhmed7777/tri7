# store_canonicalize — repair a block store whose height map has a hole

## EVIDENCE FILES (all in this bundle)

  store-canonicalize.diff              the exact diff under review
  store-canonicalize-build-test.log    cargo build + cargo test output, captured
                                       from the tree that holds that diff
  store-canonicalize-live-verify.log   the live DNS2 verification WITH FULL
                                       PROVENANCE: the exact command, the UTC
                                       and local timestamps, the sha256 of the
                                       binary AND of the reviewed source, the
                                       input files' metadata and sha256, the
                                       tool's stdout, and the exit status

## THE DEFECT (measured on DNS2)

A fork surgery left the node's blocks file containing TWO interleaved chains:
the canonical one and the debris of the abandoned branch. The bodies were
ALL present — a byte-level walk of the file's own prev links reached genesis
with zero breaks — but the index threads heights by FILE POSITION
(src/blockindex.rs build_index). The first debris frame therefore stole a
height, and every canonical frame after it failed the chaining rule and was
rejected. The height map lost heights h2201750..h2203320 (1,571 of them).

Everything downstream inherited that hole:
  - the balances replay stopped at h2201090,
  - the wallet live view could not seed ("store missing block body at
    h2207625"),
  - the explorer showed a gap in the middle of the chain,
  - the node's own getblockhash answered "not covered by the height map"
    for every height in the band, even though the body was on disk.

## THE FIX

Rewrite the file so it contains ONLY the canonical chain, in ascending
height order. No data is fetched, because no data was ever missing.

## HOW IT STAYS HONEST

1. The canonical chain is discovered from the FILE'S OWN prev links, anchored
   at the tipstate window base (a height the node already trusts). The tool
   does not ask a peer and does not trust the file's frame ORDER.
2. The tipstate window must be strictly contiguous and ascending, and its
   height RANGE must not overflow u64. Any of those ABORTS the run before
   anything is written.
3. The chain below the window must cover EVERY height from genesis up to the
   window base. A short walk means the history is genuinely incomplete, and
   the tool ABORTS rather than writing a partial chain.
4. THE GENESIS ANCHOR is enforced in EVERY case: the first canonical frame
   must have an all-zero prev link. This holds whether the window begins at
   genesis (the first frame IS the window base) or above it (the first frame
   is the block below the window base, reached by the walk).
5. Every frame is re-verified before it is written: the next frame's prev
   link must equal the previous frame's HASH. A missing frame or a link
   break ABORTS the run. Nothing is silently dropped and nothing is
   silently re-anchored.
6. A TRUNCATED frame header or body is distinguished from a clean EOF and
   aborts the run.
7. DUPLICATE HEADER HASHES are resolved, not ignored. The DNS2 store really
   does contain them (the live run reports 56). A duplicate is only
   accepted when the two frames are BYTE-IDENTICAL; the same header with a
   different body is ambiguity the tool refuses to resolve, and it aborts.
8. SOURCE-CHANGE DETECTION, SCOPED TO WHAT IS ACTUALLY PROVEN:
   - the source file's LENGTH and MODIFICATION TIME are recorded before the
     scan and re-checked before the copy, so an APPEND or a REPLACEMENT of
     the file aborts the run;
   - the scan records a sha256d digest of every CANONICAL frame body; the
     copy re-computes it and the final verification re-computes it against
     the output on disk, so a MUTATION OF A COPIED FRAME aborts the run;
   - the output is additionally compared BYTE FOR BYTE with the source frame
     it came from, as the source exists at verification time.
   The claim is NOT "the whole source is frozen": a same-size in-place
   mutation of an EXCLUDED frame is not detected, because excluded frames
   are never copied and the output does not depend on them. That behaviour
   is stated in the tool's own doc header and covered by a test.
9. The output is created with create_new (never overwrites), and the output
   path is rejected if it resolves to the same file as the input.
10. The output re-walk also re-checks the genesis anchor ON THE OUTPUT and
    requires the final frame's hash to equal the tipstate window tip.
11. Unknown flags are rejected and the positional argument count must match
    exactly (an extra argument, or an output path with --verify-only, is a
    usage error).
12. The doc header states that a FAILED run may leave a partial output
    behind, which must be removed before a retry.

## THE COUNTS ARE REPORTED PRECISELY

"Dropped" can mean three different things, so the tool states each count
separately, and the live run's numbers reconcile exactly:

    physical frames in the source file: 2214605
    distinct header hashes in the source file: 2214549
    byte-identical duplicate frames: 56
    canonical frames kept: 2207967 (genesis..h2203367..tip)
    excluded distinct hashes (non-canonical, i.e. the fork debris): 6582
    excluded physical frames (debris + byte-identical duplicates): 6638

  2214549 distinct = 2207967 canonical + 6582 debris          (checks out)
  2214605 physical  = 2214549 distinct + 56 duplicates        (checks out)
  6638 excluded physical = 6582 debris + 56 duplicates        (checks out)

## FOCUSED TESTS (in the same file)

    $ cargo test --release --bin store_canonicalize
    test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

  - genesis_window_anchor_with_nonzero_prev_is_refused
  - a_window_that_begins_at_genesis_is_accepted
  - a_truncated_header_is_not_a_clean_eof
  - a_truncated_body_is_not_a_clean_eof
  - a_missing_window_frame_aborts
  - a_broken_window_link_aborts
  - a_non_contiguous_window_aborts
  - an_incomplete_history_aborts
  - debris_is_dropped_and_the_canonical_chain_survives
  - genuine_fork_debris_is_excluded_and_the_chain_stays_intact
  - a_source_append_during_processing_is_detected
  - a_mutation_of_an_excluded_frame_does_not_affect_the_output
  - a_source_mutation_between_scan_and_copy_is_detected
  - the_source_is_never_modified
  - the_output_is_never_overwritten
  - an_identical_duplicate_frame_is_accepted
  - a_duplicate_hash_with_a_different_body_aborts
  - a_window_height_that_overflows_u64_aborts

The fork-debris test builds a frame on a REAL abandoned branch (it builds on
a canonical block but its header differs, so its hash differs from every
canonical frame), then asserts the debris is EXCLUDED from the output, that
the output holds exactly the canonical frames, and that the output still
chains back to genesis.

## LIVE DNS2 MEASUREMENT

See store-canonicalize-live-verify.log for the command, the timestamps, the
binary and source hashes, the input file metadata and sha256, the tool's
stdout, and the exit status (0).

## COMPILATION AND THE LIB SUITE

From store-canonicalize-build-test.log:

    cargo build --release --bins                   -> Finished
    cargo test --release --bin store_canonicalize  -> 18 passed; 0 failed
    cargo test --release --lib                     -> 507 passed; 0 failed;
                                                      1 ignored

## FILES

  src/bin/store_canonicalize.rs — the tool and its 18 focused tests
  examples/probe_store_canon.rs — the separate read-only diagnostic that
    proved the bodies were present (not part of the repair path)

## HOW TO REVIEW

Read src/bin/store_canonicalize.rs. The critical sections are:
  - read_frame_header / read_frame_body: clean EOF vs truncation,
  - the flag and argument-count guards,
  - load_window: window contiguity and the u64 range check,
  - scan_frames: the duplicate-hash byte comparison and the frame digest,
  - canonical_order: the backward walk, the height-coverage check, the link
    verification, the genesis anchor (including window_base == 0),
  - write_canonical: the header re-read, the digest re-check, create_new,
  - verify_output: the per-frame hash cross-check, the byte-for-byte source
    comparison, the digest re-check, the link check, the length-before-slice
    check, the genesis re-check, the tip check,
  - source_identity: the length/mtime check that catches an append.
