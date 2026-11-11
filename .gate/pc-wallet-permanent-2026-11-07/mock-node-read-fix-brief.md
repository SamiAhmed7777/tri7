# The loopback stand-in node reads the request fully (macOS CI fix)

The macOS runner failed gui::tests::a_changed_fee_that_fails_is_retried_when_
the_node_returns with "the GUI must call settxfee, got: " - the stand-in
server's single read() returned an EMPTY buffer (the client's bytes had not
arrived yet), so the assertion fired for the wrong reason and the client then
saw no answer.

Fix (test-only, src/gui/tests.rs): the stand-in now reads until the JSON body
is complete (or a 5s deadline), accumulating chunks, and only then asserts
that the request really is settxfee. The server also runs until the test
stops it (a stop flag) instead of an idle timeout that expired on a loaded
runner.

No production code changes. cargo test --release --lib: 498 passed, 0 failed.
