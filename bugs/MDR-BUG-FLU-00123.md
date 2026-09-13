# MDR-BUG-FLU-00123 — AVC444 animation flickers between 4:2:0 and chroma-refined output

- **State:** Closed
- **Priority:** Must
- **Severity:** Medium
- **Area:** graphics/avc444
- **Raised:** 2026-08-26T07:01:57Z
- **Discovery source:** Human
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
- **State history:** Open (2026-08-26T07:01:57Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-26T07:26:26Z, deltic:auto role=fix run=fix-20260826T070341Z-002f10ac branch=task/bug-MDR-BUG-FLU-00123-run-fix-20260826T070341Z-002f10ac code=f96fe83 gate=manual) -> Closed (2026-09-13T06:16:59Z, 0x4D44/Codex verify run=verify-20260913T060703Z-9ed3a291)

## Observation

Arthur reports that the earlier persistent chroma degradation is fixed, but animated regions still show a repeatable chroma flicker. The visible shape matches an LC=1 main-view update being presented as required 4:2:0 output, followed by a separately presented LC=2 chroma refinement; a subsequent luma frame then returns the region to 4:2:0. Chroma-only refinement needs presentation debouncing so it settles after motion without delaying fresh luma or retaining stale auxiliary samples.

## Fix

Commit `f96fe8373d19699fa935be2648f2c9d1c930503c` marks AVC444 chroma-only updates as deferred and releases a stale refinement deadline when a surface becomes visible again. The focused regressions `surface::tests::revealing_a_surface_releases_its_chroma_refinement_delay` and `gfx::tests::chroma_refinement_metadata_reaches_the_surface_presenter` passed; the worker's `chroma` filter passed 10 tests. As a behavioral red check, making `SurfaceStore::release_presentation_delay` a no-op made the reveal regression fail its own assertion (`left: Some(Instant { ... })`, `right: None`); the source was restored and its diff is empty. `cargo build --locked`, `cargo test --locked` (908 library, 17 binary, 8 integration tests), `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/check-windows.sh --locked`, and `./scripts/test-vendored.sh` (372 vendored tests) all passed. No live RDP or physical-display reveal test was available for this verification pass.

## Notes
