# MDR-BUG-FLU-00047 — AVC444 v1 drops chroma on unaligned surface widths

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** gfx/avc444
- **Raised:** 2026-08-23T12:08:41Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T095003Z-f482c1cb
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00047-run-verify-20260913T095003Z-f482c1cb
- **Owner base:** 4e06ab346de6f85ff69a14c857f3089b55bfe85a
- **Owner fingerprint:** sha256:b6fe085778b2533c7afde57478c44b1652aae8521c037915e61b29a2de5616ca
- **Owner since:** 2026-09-13T09:50:03Z
- **Owner until:** 2026-09-13T11:50:03Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T12:08:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@max) -> Fixed (2026-08-23T12:14:10Z, deltic:auto role=fix run=fix-20260823T120917Z-08d8da0e branch=task/bug-MDR-BUG-FLU-00047-run-fix-20260823T120917Z-08d8da0e code=e9fd732 gate=manual)

## Observation

The AVC444 client applies the AVC444v2 align32 width requirement to AVC444 v1 before either combined or chroma-only passes. A v1 frame decoded at an ordinary unaligned surface width, such as 1366, is therefore rejected on every chroma pass even though v1 packing uses row blocks and does not require the v2 horizontal split. The client should require surface width for v1 and align32(surface width) only for v2, while keeping the existing height and well-formedness checks.

## Fix

<unfixed — raised only>

## Notes
