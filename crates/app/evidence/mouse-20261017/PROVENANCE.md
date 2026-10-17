# Mouse integration render smoke

Actual debug app rendered/reopened four live PTYs in the private
Weston/Pixman/Lavapipe namespaces. Exact executable, font and SDK hashes are
in run.json. Root visually inspected the PNG: distinct child ACKs, Unicode
and recovery warning were present. No pointer/keyboard injection occurred.
This capture precedes the later explicit-paste integration and does not prove
mouse native delivery, clipboard operation, GPU or platform acceptance.
The accompanying pure and real-PTY tests cover wire encoding separately.
