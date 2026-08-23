# MDR-BUG-FLU-00086 — Overhanging sparse ClearCodec tiles replace untouched edge pixels with black

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/clearcodec-rendering
- **Raised:** 2026-08-23T22:33:58Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T225616Z-2e30450d
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00086-run-fix-20260823T225616Z-2e30450d
- **Owner base:** f8b646eed0596f360b6b1f604769af1189644505
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T22:56:16Z
- **Owner until:** 2026-08-24T00:56:16Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:33:58Z, raised via `deltic bugs new` model=gpt-5.6-sol@max)

## Observation

GfxHandler seeds ClearCodec with Surface::extract(destination), which clips an overhanging destination and returns a shorter buffer. ClearCodecDecoder rejects that seed length and starts from zeros, so sparse layers leave visible in-bounds edge pixels black when the full decoded rectangle is blitted back. Preserve a full destination-sized seed with clipped surface pixels at their correct stride, and regress a sparse right/bottom overhang.

## Fix

<unfixed — raised only>

## Notes
