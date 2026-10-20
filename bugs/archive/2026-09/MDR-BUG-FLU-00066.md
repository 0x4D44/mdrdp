# MDR-BUG-FLU-00066 — Native sessions cannot unlock the Windows PIN desktop and all input is rejected

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/auth-input
- **Raised:** 2026-08-23T20:27:23Z
- **Discovery source:** Human
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
- **State history:** Open (2026-08-23T20:27:23Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T21:37:56Z, deltic:auto role=fix run=fix-20260823T205857Z-c8a7b1ba branch=task/bug-MDR-BUG-FLU-00066-run-fix-20260823T205857Z-c8a7b1ba code=bae02ec gate=manual) -> Closed (2026-09-13T05:45:26Z, independent verifier: input synchronization tests, the full 409-test Rhydra suite, and the Windows cross-target gate passed; removing the retry synchronization failed the exact call-sequence assertion, model=codex@max)

## Observation

On Quench at 5120x2880/200%, a native connection showed the Windows PIN screen, unlike an ordinary Remote Desktop connection, and keyboard and pointer input did nothing. The contemporaneous rhydra-server log recorded SendInput injected 0 of 1 for every mouse, button, and scancode event although the control health ladder reported input-desktop ok. Expected: native connection performs an authenticated Windows logon/unlock and accepts input, or refuses before presenting an unusable locked session.

## Fix

Commit `bae02ec840cfb04a3a40475c49ebc47c3a1dc560` moves the agent into a LocalSystem
service that follows the active console session and synchronizes the input thread to
the current Windows input desktop before injection. The selected synchronization,
retry, fast-path, and trusted-peer tests passed; removing only the `sync()?` before the
retry made `rejected_input_synchronises_and_retries_exactly_once` fail with
`["send", "send"]` instead of `["send", "sync", "send"]`. The full server suite
passed 409/409 and `scripts/check-windows.sh --locked` passed for both targets.

The original locked-PIN Quench observation was reviewed against the service and input
paths, but this macOS pass did not perform a new Windows secure-desktop or live Quench
session.

## Notes
