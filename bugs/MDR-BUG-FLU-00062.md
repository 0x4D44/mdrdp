# MDR-BUG-FLU-00062 — Clipped SurfaceToSurface updates can paint nothing or leave unannounced partial pixels

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/display-surface
- **Raised:** 2026-08-23T12:44:12Z
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
- **State history:** Open (2026-08-23T12:44:12Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T19:46:27Z, deltic:auto role=fix run=fix-20260823T193108Z-286b9a76 branch=task/bug-MDR-BUG-FLU-00062-run-fix-20260823T193108Z-286b9a76 code=182f3cf gate=manual) -> Closed (2026-09-13T11:27:36Z, 0x4D44/Codex verify run=verify-20260913T111922Z-862bf4fc)

## Observation

SurfaceStore clips an overhanging source rectangle when extracting pixels but continues to use the requested width and height for source stride and destination checks. The blit can reject a valid clipped copy or partially mutate an early destination before returning without a generation bump. Use one clipped source rectangle consistently for extraction, filtering, stride, and all destinations.

## Fix

SurfaceStore clips the source rectangle before extraction, derives the copied width and height from that clipped rectangle, and uses those dimensions for every destination stride before advancing the generation after successful writes.

## Notes

## Verification

The verification build is commit 9daf83c5b6eae62bda818c2801815ece394d8044 and contains the full fix commit 182f3cf50560d164b0ed50217301fe9d326ae5c0. The current implementation clips and extracts the source at src/surface.rs:1517, then uses the clipped dimensions for every destination.

The lead focused command cargo test --locked --lib surface::tests::surface_to_surface_uses_clipped_source_dimensions_for_all_destinations -- --exact passed: 1 passed, 0 failed, 907 filtered out. It copies a source rectangle overhanging a 4 by 4 source to two destinations, checks every copied pixel, and checks that the visible generation advances. An independent verifier also ran this regression, an overhanging source-stride test, and the Gfx surface/cache copy test: 3 passed, 0 failed.

As a root behavioral mutant, I changed only the copied dimensions from clipped_src.width and clipped_src.height back to src_rect.width and src_rect.height. The same regression failed with exit 101: a clipped source rectangle should copy to every destination: ShortSource { needed: 64, got: 16 }. Restoring the source made the regression pass 1/1 and git diff --exit-code for src/surface.rs returned 0. The independent verifier reproduced the same ShortSource failure and confirmed restoration.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live RDP/runtime display session was exercised, so this closure relies on the deterministic SurfaceStore and Gfx unit tests plus source-level mutant evidence.
