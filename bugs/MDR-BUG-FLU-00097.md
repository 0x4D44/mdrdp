# MDR-BUG-FLU-00097 — AVC444 split chroma rectangles lose supplied detail

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/AVC444
- **Raised:** 2026-08-24T11:06:32Z
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
- **State history:** Open (2026-08-24T11:06:32Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T11:19:25Z, deltic:auto role=fix run=fix-20260824T110708Z-fad9c8b0 branch=task/bug-MDR-BUG-FLU-00097-run-fix-20260824T110708Z-fad9c8b0 code=abf5599 gate=manual) -> Closed (2026-09-14T07:16:11Z, 0x4D44/Codex verify run=verify-20260914T064734Z-aaf23e3b)

## Observation

Two adjacent auxiliary chroma rectangles can collectively cover one 2x2 luma block, but avc444.rs marks chroma_seen only when one rectangle covers the whole block. A following luma update treats that block as missing chroma and overwrites the valid auxiliary U/V samples with replicated 4:2:0 averages, causing colour leakage and fuzzy coloured edges.

## Fix

`vendor/ironrdp-graphics/src/avc444.rs:223-300` now accumulates partial auxiliary coverage across adjacent rectangles and promotes a 2x2 block only when its in-surface samples are complete (`1d72c9f`, integrated by `abf5599`).

## Verification

Independent verifier ran `avc444::tests::split_chroma_regions_are_collectively_replaced_by_a_luma_block` and the AVC444 family; respectively 1 and 20 tests passed. Replacing regional coverage accumulation with zero made the focused test fail at `vendor/ironrdp-graphics/src/avc444.rs:842` on `buf.chroma_seen_at(1, 1)`; restoration returned both focused and family runs to green with a clean worktree.

Lead verifier reran the split-chroma regression through `vendor/ironrdp-graphics/Cargo.toml`, where 1 test passed. The same root mutation failed at `vendor/ironrdp-graphics/src/avc444.rs:842`; restoration passed the regression again.

All repository gates passed on tree `5efa6e7246357e7a6057845a31b8219eb3f40668`: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP interoperability session was run; the vendored AVC444 unit regression exercises the recorded split-coverage loss directly.
