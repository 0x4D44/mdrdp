# MDR-BUG-FLU-00089 — LC2-first AVC444 sequence seeds a fake chroma average and suppresses later LC1 detail

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** graphics/avc444
- **Raised:** 2026-08-24T08:33:16Z
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
- **State history:** Open (2026-08-24T08:33:16Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T08:45:12Z, deltic:auto role=fix run=fix-20260824T083347Z-fde8063d branch=task/bug-MDR-BUG-FLU-00089-run-fix-20260824T083347Z-fde8063d code=ef26b75 gate=manual) -> Closed (2026-09-13T14:34:33Z, 0x4D44/Codex verify run=verify-20260913T140818Z-d830d62c)

## Observation

Yuv444Buffer initializes chroma_confirmed_avg to neutral 128 and mark_chroma_seen records that value when LC2 arrives before any luma. A following LC1 frame with a real main-plane average can therefore mark valid LC2 chroma stale and flatten odd-position detail. Reproduce with a full-frame apply_chroma_v2 first, then uniform LC1 luma whose U/V average differs from 128; the odd pixel must retain its auxiliary chroma detail.

## Fix

The recorded source fix is `8a1ac8ebf743290c9b4c8055147d2ab33e87e06`, followed by
the integration version bump `ef26b75b0a9cdc23444258b902fef82669c62b12`. It added
`luma_avg_seen` so an LC2-first packet could not treat the neutral initialization as
a real luma-confirmed average. Later `edb652652fbd77f2aff53d140408171ac8c18b87`
started rejecting chroma-only frames without a luma baseline, and
`dea775e713bbb52be6d324608f37d292becf3d48` replaced the average-tracking design:
current luma updates use the main view's chroma and current LC2-only frames without
a baseline are rejected. That supersedes the old expectation that LC2 detail must
survive the first LC1 packet.

## Notes

## Verification

The historical source-fix tree was commit `8a1ac8ebf743290c9b4c8055147d2ab33e87e06`.
The lead and independent verifier each ran
`avc444::tests::an_lc2_first_sequence_keeps_detail_after_the_first_luma_average`;
it passed 1/1. The lead root mutant changed
`if !seen || self.luma_avg_seen[word] & mask == 0` to `if !seen`; the regression
failed at historical `vendor/ironrdp-graphics/src/avc444.rs:654` on its own
“first real luma average must establish the baseline” assertion. Restoring the
condition made it pass 1/1, and the historical source diff and status were clean.

The current verification tree was commit `2746987e4b664bec1d7d316ee209e02027d03b54`.
The replacement tests
`a_luma_pass_replaces_previously_delivered_chroma_detail`,
`a_luma_view_wins_even_when_auxiliary_chroma_arrived_first`,
`a_chroma_pass_restores_detail_after_the_main_view`,
`avc444_lc1_applies_the_main_view_only_inside_its_rects`, and
`avc444_lc2_without_luma_baseline_is_skipped` each passed 1/1.

Lead mutants against the current replacement were all red on their own assertions:
preserving old odd-position chroma failed at
`vendor/ironrdp-graphics/src/avc444.rs:824`; omitting `clear_chroma` failed at
`vendor/ironrdp-graphics/src/avc444.rs:823`; removing the initial luma-baseline
guard failed at `vendor/ironrdp-egfx/src/client.rs:3258`; and bypassing both
baseline filters produced a forbidden 64x48 `ChromaRefinement` update at
`vendor/ironrdp-egfx/src/client.rs:3240`. Restoring each mutation made its
regression pass, with source diff and `git diff --check` clean afterward.

The six repository gates all exited 0 on the current verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The root suite passed 908 library, 17 binary,
5 integration, and 3 ZGFX tests; the vendored suites passed 638 tests. The Windows
gate emitted the existing unused `width`/`height` warnings in `src/present.rs`.
No live RDP session was available because `quench.lan.example` failed DNS resolution;
these closure decisions use deterministic codec/client tests and explicitly retire
the historical detail-preservation expectation through the current replacement.
