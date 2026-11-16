# CI macOS socket-test flake — root cause + fix (test-only)

## Trigger
CI run 43548146764 (cf4ba9cd, item-24 base fix) job `macos-build / Run lib tests`
FAILED: 780 passed, 2 failed:
- node_embedded::tests::adopted_body_getdata_arrives_on_the_advertisers_socket_not_the_outbounds
- node_embedded::tests::aged_body_requeue_rotates_off_the_silent_advertisers_socket
Panic lines: 17199 "the advertiser socket must receive the getdata frame",
17254 "the rotated target's socket must receive the getdata" — i.e. the read
returned 0 bytes. Linux-tests + windows-build jobs green on the same commit;
local suite on DNS2 (Linux) green both before and after.

## Root cause (platform socket semantics, NOT timing luck)
Both tests: bind 127.0.0.1:0 listener -> set_nonblocking(true) on the LISTENER
-> connect up front -> send() getdata on the accepted session -> accept() ->
set_read_timeout(300ms) -> ONE read(), unwrap_or(0).
BSD/macOS: an accepted socket INHERITS the listener's O_NONBLOCK (Linux does
not). On the macOS runner the accepted socket is therefore non-blocking; if
the frame has not landed in the receive buffer at the instant of the read,
read() returns WouldBlock immediately, unwrap_or(0) maps it to n=0, and the
n>0 assertion fails. Whether the frame arrived first is a pure race — hence
intermittent (the item-22 "macos flake rerun" episode). Linux CI always
inherits a BLOCKING accepted socket, so the 300ms timeout actually bounds the
read and the tests pass deterministically there.

## Prior art already in the tree (this fix follows it)
src/gui/tests.rs:283 (from the earlier macOS-runner failure):
  "On BSD/macOS an accepted socket INHERITS the listener's O_NONBLOCK (Linux
   does not) - without this the read below returns immediately with no data."
  s.set_nonblocking(false).expect("blocking socket");
The two failing tests are NEWER (item 21 r2 / item 23) and missed that guard.

## Fix (test-only, 3 accepted-socket sites across the 2 tests)
After every accept() in the two tests: sock.set_nonblocking(false) before
set_read_timeout/read. On Linux: a no-op (already blocking). On macOS: makes
the 300ms SO_RCVTIMEO meaningful and the read deterministic. Production code
untouched (all production platforms are Linux/Windows; the nonblocking
P2P listener at node_embedded.rs:12210 is daemon code with its own error
handling, not this pattern).

## Verification
- Both tests pass locally (Linux) before AND after (they were deterministic-
  passing on Linux; the change is inert there).
- Full lib suite re-run on the patched tree.
- CI on the fix commit must show macos-build green.
