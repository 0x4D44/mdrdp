# MDR-BUG-FLU-00126 — Native ACK writer failure suppresses the window close notification

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** native/session-lifecycle
- **Raised:** 2026-09-04T22:00:20Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T103327Z-fe5ba486
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00126-run-verify-20260914T103327Z-fe5ba486
- **Owner base:** 838082d97f4ec1af2de3134dcbaf1d26793b64fe
- **Owner fingerprint:** sha256:e5fabec34ececd68edf350e8da70b12689f0b9504658a14d664e1b0ad061b036
- **Owner since:** 2026-09-14T10:33:27Z
- **Owner until:** 2026-09-14T12:33:27Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-09-04T22:00:20Z, raised via `deltic bugs new`) -> Fixed (2026-09-05T06:19:18Z, deltic:auto role=fix run=fix-20260905T060401Z-97098776 branch=task/bug-MDR-BUG-FLU-00126-run-fix-20260905T060401Z-97098776 code=e6f5e55 gate=manual)

## Observation

Source-confirmed at baseline `7ef48e989010f7df0474d3ff5d331bb5152561f6`; not reproduced by running the app or tests.

An acknowledgement socket write failure can leave the native session window open with frozen pixels. In `~/language/mdrdp/src/native/session.rs:173`, `run_ack_writer` sets the shared stop flag before shutting down the video socket. It does not notify the window. The video reader treats an already-set stop flag as `SessionEnd::WindowClosed` (`:1032`), and the video/input completion handlers suppress window-close notification once stop is set (`:985`, `:1012`). The event loop still waits in `~/language/mdrdp/src/main.rs:1821`; shutdown follows window exit, not worker completion.

Trigger sequence for a regression: keep the window/session active, make the ACK writer fail before either reader reports a transport failure, and let the video reader observe the resulting socket shutdown. Expected: a close notification and a transport-failure result. Actual by source: no close notification and the error can be classified as an intentional close.

## Fix

Integrated as `e6f5e55`, version 0.1.241, on 2026-09-05. ACK failure now atomically claims terminal notification, shuts down the socket, and returns its reason to native teardown as `TransportFailed`. Video/input completion shares the same claim, so competing failures notify once. Intentional shutdown remains quiet.

Regression evidence: restoring the old ACK behavior failed the expected `TransportFailed` assertion. Replacing the shared claim with unconditional success failed intentional-shutdown classification and observed three notifications instead of one. Mutations were restored; all 60 native-session tests passed. Windows type-check, formatting, library/test clippy, and CLI help smoke passed. The contention test passed 20 repeat runs. No live GUI disconnect was tested.

Detailed evidence: [native review fix journal](~/language/mdrdp/wrk_journals/2026.09.05%20-%20JRN%20-%20native%20review%20fixes.md). Fixed, awaiting independent verification; this fixing session does not close its own record.

## Notes

Proposed fix (small, approximately 1–2 hours): give ACK failure the same terminal notification contract as the other workers, retaining its failure reason separately from intentional shutdown. Ensure exactly one terminal notification under competing failures.

Add a deterministic ACK-write failure regression that observes the close event and final `SessionEnd`, plus an intentional-shutdown control case. The existing test at `~/language/mdrdp/src/native/session.rs:5640` checks the stop flag, not that the window is notified; it was read, not run.

Related but distinct from fixed MDR-BUG-FLU-00107: that record repairs the video/input worker paths. This record covers the independent ACK writer setting stop first. No fix made in this review.
