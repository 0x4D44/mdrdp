# MDR-BUG-FLU-00053 — Spike viewer copies the full 5K canvas for each small rect update

- **State:** Closed
- **Priority:** Could
- **Severity:** Medium
- **Area:** rhydra/measurement-viewer
- **Raised:** 2026-08-23T12:09:37Z
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-25T06:56:34Z, deltic:auto role=fix run=fix-20260825T063002Z-25dbfdab branch=task/bug-MDR-BUG-FLU-00053-run-fix-20260825T063002Z-25dbfdab code=533b3e7 gate=manual) -> Closed (2026-09-13T10:41:56Z, 0x4D44/Codex verify run=verify-20260913T102737Z-15f832fb)

## Observation

The latency-spike viewer publishes an Arc snapshot that the UI retains, then Arc::make_mut clones the full 5120x2880 RGBA canvas before applying the next small rect. That is about 56.25 MiB copied per typing-class update and materially distorts rect latency. Use a bounded mutable surface handoff that preserves snapshot coherence without full-canvas copy-on-write per rect.

## Fix

The viewer now reuses a bounded pair of coherent RGBA surfaces for rect updates. It reclaims an untaken snapshot or recycles the presenter’s prior surface, so full canvas copy-on-write is only the bootstrap fallback and displaced damage stays attached to the next frame.

## Notes

## Verification

The verification build contains fix commit 533b3e7b487be82dcff7f8b5132beb8dae37dedb. The viewer sink tests passed 23/23, including consecutive presented rect updates, the lagging presenter path, damage preservation, and shared publication.

As a root behavioral mutant, the recycled-surface extraction was changed to always return None. The test sink::tests::consecutive_presented_rect_updates_reuse_a_bounded_surface failed with exit 101 at sink.rs:1205: the presenter returns its prior surface. Restoring the exact source produced an empty diff; the same test then passed 1/1. An independent verifier reproduced the mutant failure at the pointer reuse assertion and independently confirmed restoration.

The six repository gates passed on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate exited 0 with the existing unused width/height warnings.

No live viewer, 5K window, GPU, network, benchmark, or production timing evidence was collected or claimed.
