# Pinned xim-rs preedit-start reply patch

This profile targets xim-rs commit `16f35a2c881b815a2b6cdfd6687988e84f8447d8`. It changes one dispatch branch in `src/client.rs`: after a successful `ClientHandler::handle_preedit_start`, the dispatcher sends exactly one `PreeditStartReply` with `return_value: -1` (unlimited preedit length). If the handler returns an error, the reply is not sent. The tests in the patched source use a mock `ClientCore` transport to check both cases.

The profile verifies the official pinned source archive, exact file preimage, patch hash, and postimage. The archive root is `xim-rs-16f35a2c881b815a2b6cdfd6687988e84f8447d8`. PigADE's bootstrap integration applies this profile to a private extracted dependency checkout; it does not vendor or rewrite the cached Cargo source.
