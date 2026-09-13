# MDR-BUG-FLU-00079 — Offscreen and cache-only EGFX work is counted as a presented frame

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/presentation-latency
- **Raised:** 2026-08-23T21:51:27Z
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
- **State history:** Open (2026-08-23T21:51:27Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T22:10:53Z, deltic:auto role=fix run=fix-20260823T215208Z-d4d3caae branch=task/bug-MDR-BUG-FLU-00079-run-fix-20260823T215208Z-d4d3caae code=f405b3a gate=manual) -> Closed (2026-09-13T13:05:25Z, 0x4D44/Codex verify run=verify-20260913T125304Z-3a2d3d07)

## Observation

SurfaceStore uses generation as its visible presentation-damage token, but create/delete and mutation of offscreen surfaces plus cache-only operations also advance it. Session::notify_if_painted then wakes the window, increments frame and paint statistics, copies unchanged output, and can close input-to-paint latency against unrelated work. Restrict presentation generation to visible output changes and update cache diagnostics through a separate non-presentation path.

## Fix

`f405b3a2eedaf821b835b6ed7f57fb4f38cb7709` limits presentation generation
changes to visible output and refreshes cache diagnostics separately.

## Notes

## Verification

The verification tree was commit `3dfda67d204045ff8d196716a8001730ffd83c03`,
which contains the fix commit `f405b3a2eedaf821b835b6ed7f57fb4f38cb7709`.

The lead reran `surface::tests::offscreen_and_cache_only_work_does_not_advance_presentation_generation`
and `session::tests::offscreen_and_cache_work_refresh_stats_without_painting_or_waking`;
each passed 1/1. The independent verifier reran both regressions, 62 SurfaceStore
tests, and 96 session tests; all passed.

As the lead root mutant, I added `self.touch_presentation()` to
`src/surface.rs:1580` in the cache-to-surface path. The SurfaceStore regression
failed at `src/surface.rs:2033` with generation `2` instead of `1`. The independent
verifier reproduced the same red mutant. Restoring the source made the focused
regression pass 1/1, and both `git diff --exit-code` and `git diff --check` were
clean.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The Windows gate emitted the existing
unused `width`/`height` warnings in `src/present.rs`. No live RDP or GUI runtime
was required for this unit-level accounting fix.
