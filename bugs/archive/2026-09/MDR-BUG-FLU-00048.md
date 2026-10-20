# MDR-BUG-FLU-00048 — Rhydra disconnect can leave injected keys or mouse buttons held

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/input
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T12:36:42Z, deltic:auto role=fix run=fix-20260823T122412Z-d7777dc8 branch=task/bug-MDR-BUG-FLU-00048-run-fix-20260823T122412Z-d7777dc8 code=f5f5fc9 gate=manual) -> Closed (2026-09-13T09:59:51Z, 0x4D44/Codex verify run=verify-20260913T095013Z-7e90d5eb)

## Observation

The Windows input server injects key and mouse-button transitions immediately but keeps no per-connection held-input ledger. EOF and connection errors flush only mouse-move telemetry, then accept the next client without synthesizing releases. A tunnel loss after a down record can therefore leave Windows with a stuck modifier or button; track successfully injected downs and release them on every connection exit.

## Fix

Rhydra now keeps a per-connection ledger of successfully injected virtual keys, scancodes, and mouse buttons. Matching key-up and button-up transitions remove holds, duplicate downs remain one hold, and every input connection exit attempts releases for the remaining holds before the listener accepts another peer.

## Notes

- The release-plan regression was observed red with two failed assertions before the
  implementation, then passed 2/2.
- The full Rhydra suite passed 310 tests and `scripts/check-windows.sh` passed.

## Verification

Independent verification confirmed fix commit f5f5fc958cbdf2d600997221543301998931cb56. `HeldInputs` tracks the three input classes in tools/latency-spike/server/src/input_state.rs:50-115; `handle_record` applies only successful transitions in tools/latency-spike/server/src/win/input.rs:419-506; and `release_held` runs after the input stream ends at :543-602. Failed release calls are logged explicitly.

The lead `input_state::tests` run passed 6/6. The independent verifier reproduced 6/6, ran the two release-plan tests from the fix history at 2/2, and confirmed the Windows cfg check compiles both mdrdp and Rhydra. No live Windows or input-device run was available on this macOS host.

As a red root mutant, changing `if down` to `if !down` in tools/latency-spike/server/src/input_state.rs:108 made `release_plan_tracks_unique_successful_holds` fail with `left: []` and the expected key/scancode/button releases on the right. The independent verifier separately replaced `held.push(value)` with a no-op; both release-plan tests then failed with the same empty-plan symptom. The source was restored and the focused six-test module passed again.

The six repository gates passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate exited 0 and emitted only the existing Rhydra `visual_flow` dead-code, missing icon asset, and unused `width`/`height` warnings.
