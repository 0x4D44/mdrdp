# MDR-BUG-FLU-00124 — AVC444 refinement debounce starves continuous-motion presentation

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/presentation
- **Raised:** 2026-08-26T13:20:26Z
- **Discovery source:** Human
- **Owner:** -
- **Owner role:** -
- **Owner run:** -
- **Owner host:** -
- **Owner branch:** -
- **Owner fingerprint:** -
- **Owner since:** -
- **Owner until:** -
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-26T13:20:26Z, raised via `deltic bugs new`) -> Fixed (2026-08-26T14:07:48Z, deltic:auto role=fix run=fix-20260826T132111Z-fdc36f77 branch=task/bug-MDR-BUG-FLU-00124-run-fix-20260826T132111Z-fdc36f77 code=4ba656c2588a654d2c5a250bbbe713029e476c3c gate=manual) -> Closed (2026-09-13T06:16:59Z, 0x4D44/Codex verify run=verify-20260913T060713Z-319e4141)

## Observation

Arthur reports that full-screen window dragging feels very sticky in mdrdp 0.1.237 against both Crucible and Kiln. The behavior appeared after the 100 ms AVC444 chroma-refinement settling change. Expected: fresh picture frames remain promptly visible throughout continuous motion while late chroma-only refinements settle without a 4:2:0-to-4:4:4 flash. Actual: coalesced LC1 and LC2 damage can leave only the LC2 deadline visible to the window thread, and repeated refinements move that deadline forward until motion pauses.

## Fix

Commit `4ba656c2588a654d2c5a250bbbe713029e476c3c` preserves fresh-content urgency with a presentation epoch and lets a new picture replace an older chroma-only deadline. The focused regressions `window::tests::a_coalesced_chroma_refinement_cannot_delay_unpresented_fresh_content` and `window::tests::fresh_content_bypasses_chroma_settling_but_keeps_large_canvas_cadence` passed; the worker's `chroma_refinement` filter passed 7 tests. As a behavioral red check, changing `SurfaceStore::presentation_not_before` to return the deadline unconditionally made `a_coalesced_chroma_refinement_cannot_delay_unpresented_fresh_content` fail its own assertion (`left: Err(Instant { ... })`, `right: Ok(Copied)`). The source was restored and its diff is empty. `cargo build --locked`, `cargo test --locked` (908 library, 17 binary, 8 integration tests), `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/check-windows.sh --locked`, and `./scripts/test-vendored.sh` (372 vendored tests) all passed. No live RDP or GUI presentation test was available for this verification pass.

## Notes
