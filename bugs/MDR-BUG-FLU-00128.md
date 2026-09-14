# MDR-BUG-FLU-00128 — Partial auxiliary startup leaves detached workers without cleanup ownership

- **State:** Fixed
- **Priority:** Could
- **Severity:** Low
- **Area:** native/startup
- **Raised:** 2026-09-04T22:01:28Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T105004Z-2be849d7
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00128-run-verify-20260914T105004Z-2be849d7
- **Owner base:** d5aa486f6c1a62f955e376975e7c46445a830651
- **Owner fingerprint:** sha256:6196cfb330ea852f3b800c8c97469ba45762fc4444b5d273c3068930be49784c
- **Owner since:** 2026-09-14T10:50:04Z
- **Owner until:** 2026-09-14T12:50:04Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-09-04T22:01:28Z, raised via `deltic bugs new`) -> Fixed (2026-09-05T07:01:55Z, deltic:auto role=fix run=fix-20260905T063843Z-5bd66c1a branch=task/bug-MDR-BUG-FLU-00128-run-fix-20260905T063843Z-5bd66c1a code=da33f3e gate=manual)

## Observation

Source-confirmed at baseline `7ef48e989010f7df0474d3ff5d331bb5152561f6`; resource exhaustion was not induced and no tests or app were run.

`spawn_aux` starts its reader and writer before a final fallible clipboard-poll thread spawn (`~/language/mdrdp/src/native/session.rs:850`, `:900`, `:918`). If that final spawn fails, `?` returns without constructing an `AuxChannel`. The local join handles detach, socket clones remain in the workers, and the shared outbox is never closed. The caller logs the error and continues a native session without an auxiliary handle (`:548`).

In particular, the already-started writer waits on the empty outbox indefinitely. Its loop has no session-stop input (`~/language/mdrdp/tools/latency-spike/server/src/auxchan.rs:292`); only an outbox close or a write failure ends it. No poll worker exists to enqueue anything, and no auxiliary owner exists to close the outbox. Expected: a failed optional-channel setup cleans up every previously started worker. Actual: a detached worker and its resources survive for the process lifetime.

## Fix

Integrated as `da33f3e`, version 0.1.243, on 2026-09-05. AuxStartup now owns resources before the first worker starts. Any later clone/spawn failure closes the outbox and socket, sets auxiliary-local cancellation and performs bounded joins. The main session stop flag remains clear on optional-channel failure. Running audio and clipboard workers watch both cancellation flags; normal shutdown retains its existing 100 ms bounded-detach policy.

Four regressions cover socket-clone failures, post-worker spawn failures including the final clipboard poll spawn, audio-present rollback and direct guard cleanup. The lead disabled the guard's Drop cleanup: all four selected tests failed on their assertions (missing cancellation or workers not completed before return). After restoration and rebase onto current main, all 64 native-session tests passed, including the earlier ACK fix and blocked-clipboard shutdown tests. Formatting, warning-denying library/test clippy, both Windows type-checks and CLI help smoke passed. Existing unrelated Windows warnings remain. No live desktop, OS resource exhaustion or real clipboard was exercised.

Commands and assertion evidence: [native review fix journal](<~/language/mdrdp/wrk_journals/2026.09.05 - JRN - native review fixes.md>). Fixed, awaiting independent verification; the fixing session does not close its own record.

## Notes

Proposed fix (small/medium, approximately 2–4 hours): create a rollback owner before the first worker starts. On partial failure, close the auxiliary outbox and socket, cancel auxiliary-local work, and perform bounded joins. Do not set the shared session stop flag merely because the optional channel failed: video/input must still start normally.

Future regression: inject failure at each clone/thread-start boundary; check all started auxiliary workers terminate, the outbox closes, and the main session stop flag remains clear. Include the final poll-spawn failure after the writer has started. Tests were not run here.

Low severity reflects the rare OS-resource-failure trigger and one-process-per-session architecture. This does not claim a successful poll thread leaks inside `spawn_aux` (poll creation is its last fallible step), nor that outer session-spawn failure leaks across process exit. Fixed MDR-BUG-FLU-00117 bounds shutdown of successfully constructed channels; this defect occurs before such an owner exists. No fix made.
