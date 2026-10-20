# MDR-BUG-FLU-00072 — A replacement surface discards the last-good fallback after its first partial paint

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/surface-handoff
- **Raised:** 2026-08-23T20:34:25Z
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
- **State history:** Open (2026-08-23T20:34:25Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:43:53Z, deltic:auto role=fix run=fix-20260823T212026Z-10440d77 branch=task/bug-MDR-BUG-FLU-00072-run-fix-20260823T212026Z-10440d77 code=b8e10007a29ceedbbf75136b882d913ce6e05c4d gate=manual) -> Closed (2026-09-13T12:13:00Z, 0x4D44/Codex verify run=verify-20260913T120226Z-2965ae61)

## Observation

SurfaceStore retains the old painted output while a mapped replacement is unpainted, but finish_surface_mutation clears that fallback after the replacement's first pixel mutation. If the first update covers only part of the new surface, the rest is still zero-filled and immediately replaces the complete old desktop. Keep the fallback until a complete replacement frame is committed rather than treating one changed region as a complete surface.

## Fix

SurfaceStore keeps the last-good presentation fallback while a mapped replacement is incomplete. It retires the fallback and publishes a new generation only after the replacement has full coverage.

## Notes

## Verification

The verification build is commit 93c319f1c189c568f0b8abbec011ec63de0b498f and contains the full fix commit b8e10007a29ceedbbf75136b882d913ce6e05c4d. SurfaceStore preserves the fallback in src/surface.rs:884-887 and selects it while a replacement is incomplete in the presentation_frame path around line 1120; complete coverage retires it.

The lead focused commands surface::tests::a_partial_replacement_keeps_the_fallback_when_painted_before_mapping, surface::tests::a_partial_current_replacement_does_not_advance_generation_until_complete, and surface::tests::a_full_adopt_retires_the_replacement_fallback each passed: 1 passed, 0 failed, 907 filtered out. An independent verifier reran the partial replacement regression: 1 passed, 0 failed, 907 filtered out, and the full surface module passed 62 tests.

As a lead root behavioral mutant, I changed only the incomplete-fallback return guard in src/surface.rs:884 to if false. The selected test failed with exit 101 because the incomplete replacement advanced the visible generation from 1 to 2. The independent verifier changed the presentation selection check at src/surface.rs:1120 from is_complete() to is_painted(); the same regression failed while exposing partial blue and zero pixels in place of the all-red fallback. Restoring the source left an empty diff and the selected regression passed 1/1 again.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live RDP server, GUI compositor, physical display, or Windows runtime was exercised, so this closure relies on SurfaceStore presentation tests and source-level mutant evidence.
