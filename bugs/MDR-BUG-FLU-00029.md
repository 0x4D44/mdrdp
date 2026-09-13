# MDR-BUG-FLU-00029 — Native tiled H.264 swaps red and blue channels

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/video
- **Raised:** 2026-08-21T22:15:34Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T051517Z-5a0521f7
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00029-run-verify-20260913T051517Z-5a0521f7
- **Owner base:** a9de9cf4918fbc57386e31e50e5fbe97c79cb2cc
- **Owner fingerprint:** sha256:c498abfb686ce723470add24298b5b4cf53469516dfdb7e2f4341708fca663eb
- **Owner since:** 2026-09-13T05:15:17Z
- **Owner until:** 2026-09-13T07:15:17Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-21T22:15:34Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-21T22:49:47Z, deltic:auto role=fix run=fix-20260821T224248Z-4680dc52 branch=task/bug-MDR-BUG-FLU-00029-run-fix-20260821T224248Z-4680dc52 code=1b338550917ef5821a0dcd63573c7a68f7edcfd1 gate=manual)

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
