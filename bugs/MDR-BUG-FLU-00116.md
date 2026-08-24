# MDR-BUG-FLU-00116 — Recovery hold can delay malformed input and race teardown

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native/recovery-ordering
- **Raised:** 2026-08-24T18:08:43Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T180901Z-6d3af103
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00116-run-fix-20260824T180901Z-6d3af103
- **Owner base:** a56a5307104605748f04e5938b30a427575de116
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T18:09:01Z
- **Owner until:** 2026-08-24T20:09:01Z
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
