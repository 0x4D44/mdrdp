# MDR-BUG-FLU-00081 — Clipped SurfaceToCache rectangles leave EGFX cache metadata at the unclipped size

- **State:** Closed
- **Priority:** Must
- **Severity:** Medium
- **Area:** rdp/surface-cache
- **Raised:** 2026-08-23T22:02:07Z
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
- **State history:** Open (2026-08-23T22:02:07Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T22:06:14Z, deltic:auto role=fix run=fix-20260823T220228Z-01dc9cf1 branch=task/bug-MDR-BUG-FLU-00081-run-fix-20260823T220228Z-01dc9cf1 code=1bd98a8 gate=manual) -> Closed (2026-09-13T13:17:51Z, 0x4D44/Codex verify run=verify-20260913T130731Z-4fc1a88c)

## Observation

SurfaceStore clips an overhanging SurfaceToCache rectangle and stores the clipped bitmap dimensions, while GfxHandler records the original requested dimensions after any Ok result, including a fully out-of-bounds no-op. Later CacheToSurface placement checks and cache diagnostics use metadata that does not match the stored pixels, so valid placements can be skipped and diagnostics report the wrong size. Return the actual stored dimensions from SurfaceStore and update metadata only when a cache entry was written.

## Fix

`1bd98a8888f5ae9870072449acecf219740576a1` returns the clipped dimensions from
`SurfaceStore::surface_to_cache` and updates `GfxHandler` metadata only when a
cache entry was actually written.

## Notes

## Verification

The verification tree was commit `e4ddab86c9d0f1e8ec369ae2adff8b80c4c5ee33`,
which contains the fix commit `1bd98a8888f5ae9870072449acecf219740576a1`.

The lead ran `gfx::tests::clipped_surface_to_cache_records_the_bitmap_that_was_stored`;
it passed 1/1. The independent verifier reran that regression, five surrounding
tests, and the full root suite: 908 library, 17 binary, 5 integration, and 3 ZGFX
tests passed.

As the lead root mutant, I changed the metadata assignment at `src/gfx.rs:583`
from the returned clipped dimensions to `src.width()`/`src.height()`. The
regression failed at `src/gfx.rs:1600` with `Some((4, 4))` instead of
`Some((2, 3))`; the independent verifier reproduced it. Restoring the returned
dimensions made the focused test pass 1/1, and `git diff --exit-code`,
`git diff --check`, and `git status` were clean.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The Windows gate emitted the existing
unused `width`/`height` warnings in `src/present.rs`. No live RDP or GUI runtime
was needed for this deterministic cache metadata fix.
