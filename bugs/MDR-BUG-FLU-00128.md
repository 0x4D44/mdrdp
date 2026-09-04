# MDR-BUG-FLU-00128 — Partial auxiliary startup leaves detached workers without cleanup ownership

- **State:** Open
- **Priority:** Could
- **Severity:** Low
- **Area:** native/startup
- **Raised:** 2026-09-04T22:01:28Z
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
- **State history:** Open (2026-09-04T22:01:28Z, raised via `deltic bugs new`)

## Observation

Source-confirmed at baseline `7ef48e989010f7df0474d3ff5d331bb5152561f6`; resource exhaustion was not induced and no tests or app were run.

`spawn_aux` starts its reader and writer before a final fallible clipboard-poll thread spawn (`~/language/mdrdp/src/native/session.rs:850`, `:900`, `:918`). If that final spawn fails, `?` returns without constructing an `AuxChannel`. The local join handles detach, socket clones remain in the workers, and the shared outbox is never closed. The caller logs the error and continues a native session without an auxiliary handle (`:548`).

In particular, the already-started writer waits on the empty outbox indefinitely. Its loop has no session-stop input (`~/language/mdrdp/tools/latency-spike/server/src/auxchan.rs:292`); only an outbox close or a write failure ends it. No poll worker exists to enqueue anything, and no auxiliary owner exists to close the outbox. Expected: a failed optional-channel setup cleans up every previously started worker. Actual: a detached worker and its resources survive for the process lifetime.

## Fix

<unfixed — raised only>

## Notes

Proposed fix (small/medium, approximately 2–4 hours): create a rollback owner before the first worker starts. On partial failure, close the auxiliary outbox and socket, cancel auxiliary-local work, and perform bounded joins. Do not set the shared session stop flag merely because the optional channel failed: video/input must still start normally.

Future regression: inject failure at each clone/thread-start boundary; check all started auxiliary workers terminate, the outbox closes, and the main session stop flag remains clear. Include the final poll-spawn failure after the writer has started. Tests were not run here.

Low severity reflects the rare OS-resource-failure trigger and one-process-per-session architecture. This does not claim a successful poll thread leaks inside `spawn_aux` (poll creation is its last fallible step), nor that outer session-spawn failure leaks across process exit. Fixed MDR-BUG-FLU-00117 bounds shutdown of successfully constructed channels; this defect occurs before such an owner exists. No fix made.
