# The loopback stand-in node: a BSD/macOS socket-semantics fix (test-only)

Symptom: on the macOS CI runner the new fee tests failed with
"the GUI must call settxfee, got method None; body: " - the stand-in
accepted the connection but read NOTHING from it.

ROOT CAUSE (platform difference): the stand-in's listener is non-blocking
(the accept loop must not block), and on BSD/macOS an ACCEPTED SOCKET
INHERITS the listener's O_NONBLOCK flag - Linux does not. The accepted
socket was therefore non-blocking, so read() returned immediately with no
data and the request body was empty.

Fix (test-only, src/gui/tests.rs):
- the accepted socket is explicitly set back to BLOCKING
  (s.set_nonblocking(false)) right after accept - the same behaviour on
  every platform;
- the stand-in no longer asserts inside its own thread. It RECORDS what it
  saw in a shared Mutex<String> and always answers, so a failure can never
  panic the server thread and leave the client hanging;
- the TEST thread asserts the recorded request: the body after the HTTP
  headers must parse as JSON and its "method" must be exactly "settxfee"
  (the assertion also prints the method and the first 200 body characters);
- the read loop keeps the properties from the previous round: every read is
  bounded by the time left in a 10s budget, the raw bytes are accumulated in
  a Vec<u8> and decoded for parsing (never mutated in place), and the
  request counts as complete only when the body parses as JSON.

The companion evidence file (mock-node-current-state.txt) shows the current
full text of the helper and of the test that asserts on it.

NO production code changes. The 498-test result quoted here was measured in
this repository with `cargo test --release --lib`.
