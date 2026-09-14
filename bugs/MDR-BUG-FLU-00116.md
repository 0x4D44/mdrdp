# MDR-BUG-FLU-00116 — Recovery hold can delay malformed input and race teardown

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/recovery-ordering
- **Raised:** 2026-08-24T18:08:43Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260914T094926Z-7ebf6b35
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00116-run-verify-20260914T094926Z-7ebf6b35
- **Owner base:** f2e78fddc2fe275c9849bbe11c6cb2324667a92d
- **Owner fingerprint:** sha256:e05cdcfed16b444d9a158e5bf1aed50cb9b91e39a290c96aa8d0a3aa31feb22e
- **Owner since:** 2026-09-14T09:49:26Z
- **Owner until:** 2026-09-14T11:49:26Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T18:08:43Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-24T18:17:40Z, deltic:auto role=fix run=fix-20260824T180901Z-6d3af103 branch=task/bug-MDR-BUG-FLU-00116-run-fix-20260824T180901Z-6d3af103 code=2aaa266 gate=manual)

## Observation

The initial BaseReady hold correctly preserves sparse damage, but follow-up review found that cancellation can release the sparse reader before stop is visible, allowing it to block again in socket read. Static geometry errors also wait for the recovery deadline, and the diagnostic viewer still discards pre-base rectangles instead of resolving them after its first AU. Cancellation must be terminal/stop-ordered, static validation must precede the wait, and both client paths need deterministic pre-base regression coverage.

## Fix

<unfixed — raised only>

## Notes
