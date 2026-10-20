# MDR-BUG-FLU-00116 — Recovery hold can delay malformed input and race teardown

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native/recovery-ordering
- **Raised:** 2026-08-24T18:08:43Z
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
- **State history:** Open (2026-08-24T18:08:43Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-24T18:17:40Z, deltic:auto role=fix run=fix-20260824T180901Z-6d3af103 branch=task/bug-MDR-BUG-FLU-00116-run-fix-20260824T180901Z-6d3af103 code=2aaa266 gate=manual) -> Closed (2026-09-14T10:07:30Z, 0x4D44/Codex verify run=verify-20260914T094926Z-7ebf6b35)

## Observation

The initial BaseReady hold correctly preserves sparse damage, but follow-up review found that cancellation can release the sparse reader before stop is visible, allowing it to block again in socket read. Static geometry errors also wait for the recovery deadline, and the diagnostic viewer still discards pre-base rectangles instead of resolving them after its first AU. Cancellation must be terminal/stop-ordered, static validation must precede the wait, and both client paths need deterministic pre-base regression coverage.

## Fix

`2aaa266` makes recovery cancellation stop-ordered before releasing BaseReady
waiters, validates static geometry before waiting, and covers deterministic
pre-base behavior in the native paths. The change is integrated in `2aaa266`.

## Verification

The independent verifier's focused recovery and cancellation check passed, and
its native-session family passed 64 tests.

The independent verifier disabled `finish_video_worker` inside
`finish_video_and_cancel_base` at `src/native/session.rs:1192`. The focused check
failed at `src/native/session.rs:5223` because cancellation did not make the
stop flag visible. The lead made the same mutation; its focused check failed at
`src/native/session.rs:5222` with the same cancellation assertion. Restoring the
helper made the sparse recovery test, the pre-base rectangle test, and the
64-test native-session family pass again. No live RDP runtime was used.

## Notes
