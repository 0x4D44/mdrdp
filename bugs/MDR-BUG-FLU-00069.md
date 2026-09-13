# MDR-BUG-FLU-00069 — Progressive codec state survives encoding-context deletion and same-ID surface recreation

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rdp/progressive-rendering
- **Raised:** 2026-08-23T20:34:24Z
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
- **State history:** Open (2026-08-23T20:34:24Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:14:16Z, deltic:auto role=fix run=fix-20260823T205316Z-c0acb207 branch=task/bug-MDR-BUG-FLU-00069-run-fix-20260823T205316Z-c0acb207 code=0fe7a684414981355545ad167dd4ace10f8f8e4b gate=manual) -> Closed (2026-09-13T11:50:28Z, 0x4D44/Codex verify run=verify-20260913T113905Z-ec68ea04)

## Observation

GfxHandler keys ProgressiveDecoder state by surface ID to tolerate Windows context rotation, but it ignores DeleteEncodingContext and on_surface_created does not retire state when an ID is reused. A deleted context or new surface incarnation can therefore refine coefficients from the old surface, painting stale colour blocks or ghosts. Track the active wire context per surface, retire only the matching active context on DeleteEncodingContext, and always clear progressive state on same-ID CreateSurface.

## Fix

GfxHandler tracks the active progressive encoding context per surface, clears progressive state and its context mapping on surface deletion or same-ID recreation, and ignores DeleteEncodingContext messages for obsolete contexts.

## Notes

## Verification

The verification build is commit 0ef236becc141d2bce892e29bb85bc9104bcaa4b and contains the full fix commit 0fe7a684414981355545ad167dd4ace10f8f8e4b. The lifecycle tracking and matching DeleteEncodingContext guard are in src/gfx.rs:943-950; surface creation and deletion clear the state at src/gfx.rs:748 and src/gfx.rs:767.

The lead focused commands gfx::tests::deleting_an_active_progressive_context_drops_its_surface_state, gfx::tests::deleting_an_obsolete_rotated_context_keeps_live_progressive_state, gfx::tests::recreating_a_surface_id_clears_progressive_state_and_context_mapping, and gfx::tests::deleting_one_surface_context_does_not_touch_another_surface each passed: 1 passed, 0 failed, 907 filtered out. An independent verifier ran those lifecycle regressions plus gfx::tests::deleting_a_surface_drops_its_progressive_tile_state: all 5 passed.

As a root behavioral mutant, I changed only the active-context comparison in src/gfx.rs:947 to if true. The obsolete-context regression failed with exit 101: the progressive context count was 0 instead of the expected 1 after deleting the rotated obsolete context. The independent verifier also removed the CreateSurface cleanup and got the expected same-ID recreation failure. Restoring the source made the selected test pass 1/1 and git diff --exit-code for the source returned 0; the full lifecycle set remained green.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live RDP session, Windows runtime, or network protocol stream was exercised, so this closure relies on the portable GfxHandler lifecycle tests and source-level mutant evidence.
