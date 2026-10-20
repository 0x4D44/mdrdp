# MDR-BUG-FLU-00132 — clippy -D warnings red on rust 1.98.1: chunks_exact_to_as_chunks at 30 existing sites

- **State:** Fixed
- **Priority:** Should
- **Severity:** Low
- **Area:** build
- **Raised:** 2026-10-14T23:07:35Z
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
- **State history:** Open (2026-10-14T23:07:35Z, raised via `deltic bugs new --land` model=claude-opus-5-5@medium) -> Fixed (2026-10-20T22:59:20Z, deltic:auto role=fix run=fix-20261020T225302Z-3c21586f branch=task/bug-MDR-BUG-FLU-00132-run-fix-20261020T225302Z-3c21586f code=102f9fc7549043240a5f55118d115ffc391e141e gate=manual)

## Observation

`cargo clippy --all-targets -- -D warnings` fails on origin/main (d68981d) with rustc/clippy 1.98.1 on flux: the new lint `clippy::chunks_exact_to_as_chunks` fires 30 times on existing `chunks_exact(N)` / `chunks_exact_mut(N)` calls with constant N.

Sites: src/audio.rs:159,184; src/clipboard.rs (6); src/dock.rs:489; src/gfx.rs:1200,1205,1899,1926; src/present.rs (2); src/probe/glass/detect.rs:394,425,428 (+3); src/surface.rs:2070,3820,3842 (+5). Also the vendored ironrdp-connector connection.rs:775 unfulfilled `#[expect(single_use_lifetimes)]` warning (warning only).

Observed while validating an unrelated audio-volume task; none of the sites were touched by it. The fix is mechanical (`as_chunks::<N>().0` per clippy's suggestion), but per CLAUDE.md lint suppression is not the cure.

Evidence fingerprint: `trunk-red:v2:mdrdp-clippy-d-warnings-red-on-rust-1-98-1-chun-ed9e76dc711dd574`


## Fix

<unfixed — raised only>

## Notes
