# The loopback stand-in node reads the request properly (macOS CI fix)

The macOS runner failed gui::tests::a_changed_fee_that_fails_is_retried_when_
the_node_returns with "the GUI must call settxfee, got: " - the stand-in
server's single read() did not have the complete request body yet, so the
assertion fired for the wrong reason and the client then saw no answer.

Fix (test-only, src/gui/tests.rs):
- the socket gets a 5s READ TIMEOUT, so the read loop can never block past
  its deadline (the previous draft only checked a wall-clock deadline
  BETWEEN reads, which a blocking read could outlive);
- the loop accumulates chunks and treats the request as complete only when
  the part after the HTTP headers PARSES as JSON (a '}' inside a string is
  not completion);
- the assertion then checks the parsed method equals settxfee, and reports
  the method it actually saw.

The stand-in already runs until the test stops it (a stop flag set by the
test before it joins the thread) - that part landed in the previous commit
(71bfc9a), not in this diff.

No production code changes. cargo test --release --lib: 498 passed, 0 failed
(the cited test: gui::tests::a_changed_fee_that_fails_is_retried_when_the_
node_returns).
