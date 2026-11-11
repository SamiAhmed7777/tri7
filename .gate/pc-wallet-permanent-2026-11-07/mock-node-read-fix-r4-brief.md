# The loopback stand-in node reads the request properly (macOS CI fix)

The macOS runner failed gui::tests::a_changed_fee_that_fails_is_retried_when_
the_node_returns with "the GUI must call settxfee, got: " - the stand-in
server's single read() did not have the complete request body yet, so the
assertion fired for the wrong reason and the client then saw no answer.

This diff is TEST-ONLY (src/gui/tests.rs). The companion evidence file
(mock-node-current-state.txt) shows the CURRENT full text of the helper and
of the test, so the parts that landed earlier are visible too (the server
runs until the test stops it - a stop flag the test sets before it joins the
thread).

Fix in this diff:
- every read is bounded by the time that is LEFT in a 10s budget (the socket
  read timeout is set to min(remaining, 5s) before each read), so the 10s
  ceiling is really enforced and the loop can neither block indefinitely nor
  spin;
- the raw BYTES are accumulated in a Vec<u8> and decoded for parsing - the
  buffer is never mutated in place, so a multibyte UTF-8 sequence split
  across reads cannot be corrupted;
- the request counts as complete only when the part after the HTTP headers
  PARSES as JSON (a '}' inside a string is not completion);
- the assertion checks the parsed method equals settxfee and reports the
  method it actually saw plus the first 200 bytes of the body.

NO production code changes. The 498-test result quoted here was measured in
this repository with `cargo test --release --lib` (the cited test is
gui::tests::a_changed_fee_that_fails_is_retried_when_the_node_returns).
