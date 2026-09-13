# MDR-BUG-FLU-00119 — Fixed-coordinate AVC chroma ghosts lack a final-pixel scroll regression

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** testing/graphics
- **Raised:** 2026-08-24T20:16:01Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T055436Z-2978e4e8
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00119-run-verify-20260913T055436Z-2978e4e8
- **Owner base:** d6c935f9ad02d8a73274c9503e0885a294739037
- **Owner fingerprint:** sha256:db5a04a86be5d9626d8f348e9a13195ab3fbb92efbf3e28595a49fbf12b25e94
- **Owner since:** 2026-09-13T05:54:36Z
- **Owner until:** 2026-09-13T07:54:36Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-24T20:16:01Z, raised via `deltic bugs new`) -> Fixed (2026-08-24T20:29:53Z, deltic:auto role=fix run=fix-20260824T201637Z-2b029615 branch=task/bug-MDR-BUG-FLU-00119-run-fix-20260824T201637Z-2b029615 code=7ccd8ec0d6dda020b3a5fde7c8acb4d19d32dbc1 gate=manual) -> Open (2026-08-25T10:06:20Z, 0x4D44/Codex: Arthur reproduced the same persistent fixed-coordinate chroma remnants on Kiln with mdrdp v0.1.234; the code=7ccd8ec regression does not cover the live failure) -> Fixed (2026-08-25T22:06:11Z, deltic:auto role=fix run=fix-20260825T215311Z-9b5259fd branch=task/bug-MDR-BUG-FLU-00119-run-fix-20260825T215311Z-9b5259fd code=dea775e gate=manual)

## Observation

On Kiln with mdrdp 0.1.211, purple chroma remnants remain fixed at the bottom-right of the desktop. Moving the window beneath them and scrolling its text do not move or clear the remnants. Current 0.1.214+ tests cover decoder validity state and emitted rectangles but do not reproduce a same-surface scroll/copy and assert the final RGBA pixels at the old and new coordinates.

## Fix

<unfixed — raised only>

## Notes

The v0.1.234 recurrence survived moving windows and forcing repaints. The replaced
session ended gracefully after 12,708 frames with AVC444v2 active, zero decode errors,
zero undecoded regions, and zero surface errors. This points away from a reported codec
failure and towards stale-but-valid pixel state or incomplete damage/copy coverage, but
does not yet identify which layer owns the retained remnants.

A second v0.1.234 report includes visual evidence of pale cyan and yellow glyph-shaped
remnants spread across large white regions of the desktop, behind and between current
black text and controls. The leakage preserves recognizable fragments of older text at
fixed coordinates rather than merely reducing colour resolution inside the current
update. The screenshot itself is not stored in the repository because it contains
document content.
