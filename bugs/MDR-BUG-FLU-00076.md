# MDR-BUG-FLU-00076 — Gradual AVC444 luma drift can keep stale chroma detail live indefinitely

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/avc444-chroma
- **Raised:** 2026-08-23T21:33:59Z
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
- **State history:** Open (2026-08-23T21:33:59Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

The AVC444 stale-chroma guard compares each luma-only block average with only the immediately preceding average. A sequence moving from the last aux-confirmed average in sub-threshold steps (for example 100 to 105 to 110 to 115) never marks the preserved odd-position chroma stale, even after it has drifted beyond the safe threshold. Compare the incoming average directly with the last aux-confirmed average, preserve the return-to-confirmed behavior, and prove the gradual sequence paints the flat current average until auxiliary chroma catches up.

## Fix

<unfixed — raised only>

## Notes
