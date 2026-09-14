# MDR-BUG-FLU-00110 — Native rectangle batches expose partial frames when a later rectangle is invalid

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native/frame-atomicity
- **Raised:** 2026-08-24T12:31:15Z
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
- **State history:** Open (2026-08-24T12:31:15Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T13:05:40Z, deltic:auto role=fix run=fix-20260824T125604Z-8a99767b branch=task/bug-MDR-BUG-FLU-00110-run-fix-20260824T125604Z-8a99767b code=d7b0482 gate=manual) -> Closed (2026-09-14T09:32:30Z, 0x4D44/Codex verify run=verify-20260914T091949Z-6e0bb8b8)

## Observation

NativeSink::paint applies each BGRA rectangle through SurfaceStore::blit_bgra_strict before the complete RectUpdate has been validated. If an early rectangle is valid and a later rectangle is out of bounds or has a malformed payload, the function returns an error after the earlier pixels and presentation generation have already changed, leaving a rejected mixed frame observable before teardown or another wake. Preflight every rectangle and payload, then apply the BGRA batch as one store mutation with one generation advance; prove a valid-first invalid-second update changes neither pixels nor generation.

## Fix

`ea5791d` adds `SurfaceStore::blit_bgra_strict_batch`, which preflights every
rectangle before swizzling and commits the BGRA batch with one generation
advance. The change is integrated in `d7b0482`.

## Verification

The independent verifier and lead each ran
`native::session::tests::a_malformed_later_rect_cannot_partially_paint_the_batch`;
each focused run passed one test. The native-session family passed 64 tests for
both verifiers, including the restored lead run.

The independent verifier disabled the preflight loop at `src/surface.rs:1400`.
The focused test failed at `src/native/session.rs:5496` because the valid first
rectangle changed pixels before the invalid second rectangle was rejected. The
lead made the same mutation and observed the same failure. Restoring the loop
made the focused test and 64-test native-session family pass again. No live RDP
runtime was used.

## Notes
