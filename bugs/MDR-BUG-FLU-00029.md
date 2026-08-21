# MDR-BUG-FLU-00029 — Native tiled H.264 swaps red and blue channels

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native/video
- **Raised:** 2026-08-21T22:15:34Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260821T224248Z-4680dc52
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00029-run-fix-20260821T224248Z-4680dc52
- **Owner base:** a0546d7ebd86f77acc5efc5a47ac8f9abde5856b
- **Owner fingerprint:** -
- **Owner since:** 2026-08-21T22:42:48Z
- **Owner until:** 2026-08-22T00:42:48Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-21T22:15:34Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

On 2026-08-21 Arthur connected from Flux to Quench with mdrdp v0.1.114 in
native fullscreen mode at 5120x2880. Windows colours were recognisable but red
and blue were exchanged: the blue Start button appeared yellow. Native tiled
H.264 output should preserve the Windows desktop's channel order.

Code inspection reproduces the contract violation without a live desktop:
`H264Decoder::decode` returns RGBA, while `NativeSink::on_tile_au` passes that
buffer to `SurfaceStore::blit_bgra_strict`, which swaps red and blue.

## Fix

<unfixed — raised only>

## Notes

The raw dirty-rectangle path is BGRA by wire contract and must retain its
swizzle. Only decoded tiled access units have the wrong call.
