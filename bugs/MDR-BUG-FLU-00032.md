# MDR-BUG-FLU-00032 — mdrdp deploy resets Rhydra display scale to 100% during agent reinstall

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** deploy/display
- **Raised:** 2026-08-21T23:26:41Z
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
- **State history:** Open (2026-08-21T23:26:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-21T23:45:17Z, deltic:auto role=fix run=fix-20260821T232706Z-320466c2 branch=task/bug-MDR-BUG-FLU-00032-run-fix-20260821T232706Z-320466c2 code=83deec1 gate=manual) -> Closed (2026-09-13T08:28:20Z, 0x4D44/Codex verify run=verify-20260913T081303Z-6105fc88)

## Observation

Observed on Quench while deploying integrated v0.1.117: the host was healthy at 200% scale, but the full deploy ran rhydra-agent install without display arguments. The new agent reported desired_desktop_scale_percent=100 and refused to start the server because the actual console remained at 200%. A full deploy must preserve the existing desired display mode and scale instead of silently reverting agent defaults.

## Fix

<unfixed — raised only>

## Notes

## Verification

Independent verification confirmed fix commit `83deec178e865e575d2b623ae792334a5245d685` and the complete display tuple forwarding in `src/deploy.rs:735-743`. The focused regression `deploy::tests::full_deploy_preserves_the_running_agents_complete_display_request` passed, and the restored deploy test suite passed all 36 tests. As a red root mutant, replacing the display argument expression with an empty string made the regression fail at `src/deploy.rs:2041` with `install must preserve the complete prior display tuple`. The expression was restored and the focused regression passed again.

The repository gates then passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`.

No live Quench deployment was attempted because it would change the host display mode and require remote installation. No new live claim is made. The original display-scale reset observation remains the end-to-end product observation.
