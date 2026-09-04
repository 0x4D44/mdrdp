# MDR-BUG-FLU-00126 — Native ACK writer failure suppresses the window close notification

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** native/session-lifecycle
- **Raised:** 2026-09-04T22:00:20Z
- **Discovery source:** Agent
- **Owner:** -
- **Owner role:** -
- **Owner run:** -
- **Owner host:** -
- **Owner branch:** -
- **Owner base:** -
- **Owner fingerprint:** -
- **Owner since:** -
- **Owner until:** -
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-09-04T22:00:20Z, raised via `deltic bugs new`)

## Observation

Source-confirmed at baseline `7ef48e989010f7df0474d3ff5d331bb5152561f6`; not reproduced by running the app or tests.

An acknowledgement socket write failure can leave the native session window open with frozen pixels. In `~/language/mdrdp/src/native/session.rs:173`, `run_ack_writer` sets the shared stop flag before shutting down the video socket. It does not notify the window. The video reader treats an already-set stop flag as `SessionEnd::WindowClosed` (`:1032`), and the video/input completion handlers suppress window-close notification once stop is set (`:985`, `:1012`). The event loop still waits in `~/language/mdrdp/src/main.rs:1821`; shutdown follows window exit, not worker completion.

Trigger sequence for a regression: keep the window/session active, make the ACK writer fail before either reader reports a transport failure, and let the video reader observe the resulting socket shutdown. Expected: a close notification and a transport-failure result. Actual by source: no close notification and the error can be classified as an intentional close.

## Fix

<unfixed — raised only>

## Notes

Proposed fix (small, approximately 1–2 hours): give ACK failure the same terminal notification contract as the other workers, retaining its failure reason separately from intentional shutdown. Ensure exactly one terminal notification under competing failures.

Add a deterministic ACK-write failure regression that observes the close event and final `SessionEnd`, plus an intentional-shutdown control case. The existing test at `~/language/mdrdp/src/native/session.rs:5640` checks the stop flag, not that the window is notified; it was read, not run.

Related but distinct from fixed MDR-BUG-FLU-00107: that record repairs the video/input worker paths. This record covers the independent ACK writer setting stop first. No fix made in this review.
