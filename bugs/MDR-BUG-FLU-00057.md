# MDR-BUG-FLU-00057 — Rhydra IOSurface presenter can overwrite a surface still in compositor use

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/presentation
- **Raised:** 2026-08-23T12:09:38Z
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
- **State history:** Open (2026-08-23T12:09:38Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T12:20:13Z, deltic:auto role=fix run=fix-20260823T121504Z-d89abe60 branch=task/bug-MDR-BUG-FLU-00057-run-fix-20260823T121504Z-d89abe60 code=0190ed1 gate=manual) -> Closed (2026-09-13T11:04:16Z, 0x4D44/Codex verify run=verify-20260913T105639Z-429e2c56)

## Observation

The macOS IOSurface presenter first seeks a non-last surface whose is_in_use flag is false, then falls back to any non-last surface when none is free. Writing that compositor-owned surface can produce torn rows, mixed frames, or old-frame flashes. Treat no free surface as backpressure and retry or drop safely; never overwrite an in-use surface.

## Fix

The macOS presenter now selects only a non-last IOSurface whose compositor ownership has cleared. When every safe surface is busy, it returns Busy and the window schedules a short retry instead of overwriting compositor-owned memory.

## Notes

## Verification

The verification build contains fix commit 0190ed11941f6dabd29c4040b02254c0972074c3. The focused present::tests suite passed 8/8, including the busy-pool selection and retry helpers.

As a root behavioral mutant, the safe is_in_use filter was replaced with an unconditional surface selection. The selected test present::tests::a_busy_surface_pool_never_selects_compositor_owned_memory failed with exit 101: left Some(1), right None. Restoring the exact source made the selected test pass 1/1 and left an empty source diff. An independent verifier reproduced the same assertion and confirmed the restored source.

The six repository gates passed on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate exited 0 with the existing unused width/height warnings.

The pure selection test verifies the ownership guard. No live CoreAnimation compositor, IOSurface pool, GPU, visual tearing, or frame-mixing run was available, so none is claimed.
