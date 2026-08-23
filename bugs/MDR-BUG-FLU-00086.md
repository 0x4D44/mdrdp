# MDR-BUG-FLU-00086 — Overhanging sparse ClearCodec tiles replace untouched edge pixels with black

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/clearcodec-rendering
- **Raised:** 2026-08-23T22:33:58Z
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
- **State history:** Open (2026-08-23T22:33:58Z, raised via `deltic bugs new` model=gpt-5.6-sol@max)

## Observation

GfxHandler seeds ClearCodec with Surface::extract(destination), which clips an overhanging destination and returns a shorter buffer. ClearCodecDecoder rejects that seed length and starts from zeros, so sparse layers leave visible in-bounds edge pixels black when the full decoded rectangle is blitted back. Preserve a full destination-sized seed with clipped surface pixels at their correct stride, and regress a sparse right/bottom overhang.

## Fix

<unfixed — raised only>

## Notes
