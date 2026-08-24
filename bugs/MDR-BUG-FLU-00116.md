# MDR-BUG-FLU-00116 — Recovery hold can delay malformed input and race teardown

- **State:** Open
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
- **State history:** Open (2026-08-24T18:08:43Z, raised via `deltic bugs new` model=gpt-5.6-sol@max)

## Observation

The initial BaseReady hold correctly preserves sparse damage, but follow-up review found that cancellation can release the sparse reader before stop is visible, allowing it to block again in socket read. Static geometry errors also wait for the recovery deadline, and the diagnostic viewer still discards pre-base rectangles instead of resolving them after its first AU. Cancellation must be terminal/stop-ordered, static validation must precede the wait, and both client paths need deterministic pre-base regression coverage.

## Fix

<unfixed — raised only>

## Notes
