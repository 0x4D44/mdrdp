# MDR-BUG-FLU-00082 — Rhydra cold-starts black when the Windows console is locked

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/secure-desktop
- **Raised:** 2026-08-23T22:04:30Z
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
- **Attempts:** fix=0, doubt=0, indeterminate=1
- **State history:** Open (2026-08-23T22:04:30Z, raised via `deltic bugs new`) -> Fixed (2026-08-23T22:11:32Z, deltic:auto role=fix run=fix-20260823T220531Z-aad19780 branch=task/bug-MDR-BUG-FLU-00082-run-fix-20260823T220531Z-aad19780 code=39a1844 gate=manual)

## Observation

Restarting RhydraAgent while Quench is on the Winlogon PIN desktop starts a healthy 5120x2880/200% worker and accepts secure-desktop input, but new native viewers receive a black frame and zero encoded frames. Starting unlocked and then locking captures the PIN screen correctly. Expected: a service start or restart while locked immediately captures the Winlogon desktop, so unattended reboot and recovery remain usable.

## Fix

`39a18448df197cc095042165c5b1a2d99f8b4270` makes the Rhydra reconcile thread
attach to the live input desktop before DPI setup and before it creates or replaces
children. It repeats the desktop synchronisation before each reconcile pass, so a
locked service start and later desktop transitions use the active Winlogon desktop.

## Notes

## Verification

The verification tree was commit `5fccc6f9625cac2658f10c33f7ff0c5b59b8241e`,
which contains fix commit `39a18448df197cc095042165c5b1a2d99f8b4270`. The source
changes are in `tools/latency-spike/server/src/bin/agent.rs:306-307,436-443` and
`tools/latency-spike/server/src/win/input.rs:58`.

The lead and independent verifier ran the Rhydra library suite (409/409), the
reconciler tests (38/38), desktop-sync tests (6/6), agent CLI tests (4/4), and the
Windows cross-target check. The Windows-only regression
`reconcile_thread_selects_per_monitor_v2_dpi_awareness` selected zero tests on
macOS because its module is excluded. Reversing both desktop-synchronisation root
hunks left the portable tests green and produced no locked-desktop black-frame
assertion, so the existing tests do not cover the reported runtime failure.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The Windows check emitted the existing
unused `width`/`height` warnings in `src/present.rs`.

The required locked-Winlogon Quench runtime could not be rerun from this macOS
host. The Windows-only regression was not executable here, and no live service,
SCM, viewer, frame, or encoded-output evidence is claimed. The verification claim
was cleared by the release operation; this record remains Fixed with one
indeterminate attempt pending a reachable Windows runtime check.
