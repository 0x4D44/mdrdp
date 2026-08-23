# MDR-BUG-FLU-00043 — Rhydra NV12 converter pool can grow GPU memory without bound

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/video-memory
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T133831Z-735aea9e
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00043-run-fix-20260823T133831Z-735aea9e
- **Owner base:** f265c46e547b899b6a9e4dbbac84197778888698
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T13:38:31Z
- **Owner until:** 2026-08-23T15:38:31Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

tools/latency-spike/server/src/win/convert.rs:231-255 allocates another GPU surface whenever all current surfaces remain leased. The capture loop at pipeline.rs:1056-1099 keeps converting tiles, so an MFT that accepts input without retiring output can grow the pool until GPU allocation fails. Bound the pool and apply backpressure or fail explicitly.

## Fix

<unfixed — raised only>

## Notes
