# The loopback stand-in node reads the request properly (macOS CI fix)

The macOS runner failed gui::tests::a_changed_fee_that_fails_is_retried_when_
the_node_returns with "the GUI must call settxfee, got: " - the stand-in
server's single read() did not have the complete request body yet, so the
assertion fired for the wrong reason and the client then saw no answer.

Fix (test-only, src/gui/tests.rs):
- the socket's read timeout is SET and ASSERTED (5s): every read() returns
  within it, so the loop can neither block indefinitely nor spin;
- the loop also stops after 10s of wall clock, so the total is bounded;
- the raw BYTES are accumulated in a Vec<u8> and decoded once at the end (a
  multibyte UTF-8 sequence split across reads cannot be corrupted that way);
- the request is treated as complete only when the part after the HTTP
  headers PARSES as JSON (a '}' inside a string is not completion);
- the assertion then checks the parsed method equals settxfee and reports the
  method it actually saw.

The stand-in already runs until the test stops it (a stop flag set by the
test before it joins the thread) - that part landed in the previous commit
(71bfc9a), not in this diff.

No production code changes. cargo test --release --lib: 498 passed, 0 failed
(the cited test: gui::tests::a_changed_fee_that_fails_is_retried_when_the_
node_returns).
