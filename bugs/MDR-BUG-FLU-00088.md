# MDR-BUG-FLU-00088 — ClearCodec counts unwritten coverage as painted telemetry

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** rdp/clearcodec-telemetry
- **Raised:** 2026-08-23T23:16:08Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T231638Z-d27cebf9
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00088-run-fix-20260823T231638Z-d27cebf9
- **Owner base:** 4b1aac5fdcbd1e0351f5bfd5a6c5f42d673c175a
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T23:16:38Z
- **Owner until:** 2026-08-24T01:16:38Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T23:16:08Z, raised via `deltic bugs new` model=gpt-5.6-sol@max)

## Observation

GfxHandler records the full ClearCodec destination as codec_bytes_painted even when exact coverage is empty or the surface write fails. Pixels and presentation generation stay unchanged, but diagnostics report bytes that never landed and can mislead codec and latency analysis. Account only successful in-bounds coverage returned by SurfaceStore::blit_rgba_with_coverage.

## Fix

<unfixed — raised only>

## Notes
