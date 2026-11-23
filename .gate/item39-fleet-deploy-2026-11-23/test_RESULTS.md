# item39-fleet-deploy — deterministic protocol test results

Run 2026-11-23T02:53:57Z. systemctl/cargo/RPC stubbed; bash/git/install/sha256sum/timeout/python3 real.
Harness resolves scripts and stubs relative to its own directory; all output under ./protocol-test-run/. transcripts.log holds per-scenario stdout.

**73/73 checks passed.**

| scenario | check | result | detail |
|---|---|---|---|
| D1 | exit=0 (deployed+healthy) | PASS | rc=0 |
| D1 | binary==new | PASS |  |
| D1 | backup==old | PASS |  |
| D1 | running inode==new | PASS | pid=2299023 |
| D2 | exit=1 (clean rollback) | PASS | rc=1 |
| D2 | binary restored==old | PASS |  |
| D2 | backup intact==old | PASS |  |
| D2 | running inode==old | PASS | pid=2301867 |
| D2 | unit active | PASS |  |
| D2 | poll bounded <=140s | PASS | 120.9s |
| D3 | exit=3 (pre-mutation abort) | PASS | rc=3 |
| D3 | binary untouched==old | PASS |  |
| D3 | no backup created | PASS |  |
| D5 | exit=3 (unit inactive) | PASS | rc=3 |
| D5 | no backup created | PASS |  |
| D6 | exit=3 (missing live binary) | PASS | rc=3 |
| D6 | daemon still alive | PASS |  |
| D6 | no backup created | PASS |  |
| D7 | exit=2 (FATAL rollback restart failed) | PASS | rc=2 |
| D7 | binary restored==old | PASS |  |
| D8 | exit=2 (FATAL running inode != restored) | PASS | rc=2 |
| D8 | binary path restored==old | PASS |  |
| D8 | running inode==WRONG bytes (mismatch detected) | PASS |  |
| P1 | exit=0 (deployed+healthy) | PASS | rc=0 |
| P1 | binary==new (built) | PASS |  |
| P1 | running inode==new | PASS | pid=2306148 |
| P1 | repo HEAD==pinned commit | PASS | 406be0ff344b |
| P1 | repo detached (on pinned) | PASS | detached |
| P1 | backup==old | PASS |  |
| P2 | exit=1 (clean rollback) | PASS | rc=1 |
| P2 | binary restored==old | PASS |  |
| P2 | repo HEAD==orig | PASS | 5530d0eadecf |
| P2 | repo branch restored==master | PASS | master |
| P2 | running inode==old | PASS | pid=2307863 |
| P2 | unit active | PASS |  |
| P3 | exit=3 (build failed, proven restore) | PASS | rc=3 |
| P3 | binary restored==old | PASS |  |
| P3 | repo HEAD==orig | PASS | 6d563967fc10 |
| P3 | repo branch==master | PASS | master |
| P3 | daemon NEVER restarted (same pid) | PASS | pre=2307976 post=2307976 |
| P3 | running inode==old | PASS |  |
| P4 | exit=3 (dirty tree refused) | PASS | rc=3 |
| P4 | binary untouched==old | PASS |  |
| P4 | no backup created | PASS |  |
| P4 | dirty tree left as-is | PASS |  |
| P5 | exit=3 (checkout failed, proven restore, no restart) | PASS | rc=3 |
| P5 | binary restored==old | PASS |  |
| P5 | repo HEAD==orig | PASS | c7bee76b046b |
| P5 | repo branch==master | PASS | master |
| P5 | daemon NEVER restarted (same pid) | PASS | pre=2308164 post=2308164 |
| P5 | running inode==old | PASS | pid=2308164 |
| P6 | exit=2 (FATAL rollback restart failed) | PASS | rc=2 |
| P6 | binary restored==old | PASS |  |
| P6 | repo HEAD==orig | PASS | a0248d922518 |
| P10 | exit=3 (fetch failed, proven restore, no restart) | PASS | rc=3 |
| P10 | binary restored==old | PASS |  |
| P10 | repo HEAD==orig | PASS | 06c324a341ff |
| P10 | repo branch==master | PASS | master |
| P10 | daemon NEVER restarted (same pid) | PASS | pre=2310311 post=2310311 |
| P10 | running inode==old | PASS |  |
| P11 | exit=2 (FATAL running inode != restored) | PASS | rc=2 |
| P11 | binary path restored==old | PASS |  |
| P11 | repo HEAD==orig | PASS | 8c7743103bac |
| P11 | running inode==WRONG bytes (mismatch detected) | PASS |  |
| P7 | exit=3 (missing live binary) | PASS | rc=3 |
| P7 | daemon still alive | PASS |  |
| P7 | repo untouched==orig | PASS |  |
| P7 | no backup created | PASS |  |
| P8 | exit=3 (backup dir not creatable) | PASS | rc=3 |
| P8 | binary untouched==old | PASS |  |
| P8 | repo untouched==orig | PASS |  |
| P9 | exit=3 (not a git repo) | PASS | rc=3 |
| P9 | no backup created | PASS |  |
