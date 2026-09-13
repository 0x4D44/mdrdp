# MDR-BUG-FLU-00088 — ClearCodec counts unwritten coverage as painted telemetry

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rdp/clearcodec-telemetry
- **Raised:** 2026-08-23T23:16:08Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T140805Z-dc1a5d4a
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00088-run-verify-20260913T140805Z-dc1a5d4a
- **Owner base:** 506f01d4cb91bb0f8e5c221ae886d19617f32075
- **Owner fingerprint:** sha256:9b660a847142ecfd27f4aaaf4ec0e53889e3f3bd7236a33cd41bc79c0c2b6580
- **Owner since:** 2026-09-13T14:08:05Z
- **Owner until:** 2026-09-13T16:08:05Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T23:16:08Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T23:20:40Z, deltic:auto role=fix run=fix-20260823T231638Z-d27cebf9 branch=task/bug-MDR-BUG-FLU-00088-run-fix-20260823T231638Z-d27cebf9 code=5210024fa35983ad03b5d0d57ca661c59b86f9a6 gate=manual)

## Observation

GfxHandler records the full ClearCodec destination as codec_bytes_painted even when exact coverage is empty or the surface write fails. Pixels and presentation generation stay unchanged, but diagnostics report bytes that never landed and can mislead codec and latency analysis. Account only successful in-bounds coverage returned by SurfaceStore::blit_rgba_with_coverage.

## Fix

<unfixed — raised only>

## Notes
