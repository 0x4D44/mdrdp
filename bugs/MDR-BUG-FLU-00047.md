# MDR-BUG-FLU-00047 — AVC444 v1 drops chroma on unaligned surface widths

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** gfx/avc444
- **Raised:** 2026-08-23T12:08:41Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T120917Z-08d8da0e
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00047-run-fix-20260823T120917Z-08d8da0e
- **Owner base:** 8658495f61bbdf421efca05bfdf13c5c5bc50389
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T12:09:17Z
- **Owner until:** 2026-08-23T14:09:17Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:08:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@max)

## Observation

The AVC444 client applies the AVC444v2 align32 width requirement to AVC444 v1 before either combined or chroma-only passes. A v1 frame decoded at an ordinary unaligned surface width, such as 1366, is therefore rejected on every chroma pass even though v1 packing uses row blocks and does not require the v2 horizontal split. The client should require surface width for v1 and align32(surface width) only for v2, while keeping the existing height and well-formedness checks.

## Fix

<unfixed — raised only>

## Notes
