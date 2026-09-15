# MDR-BUG-FLU-00078 — Rhydra service cannot move its worker token into the console session

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** Rhydra service
- **Raised:** 2026-08-23T21:46:28Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260915T063314Z-6a79e964
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00078-run-verify-20260915T063314Z-6a79e964
- **Owner base:** ea1c5e46fe44b52f33b3a5a3bdd38593ee7fc541
- **Owner fingerprint:** sha256:3ed7007b1030a01d2bb5e9517fd34a1043c6843b0290d6d0331beb684b7a2eab
- **Owner since:** 2026-09-15T06:33:14Z
- **Owner until:** 2026-09-15T08:33:14Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=1
- **State history:** Open (2026-08-23T21:46:28Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T21:48:01Z, deltic:auto role=fix run=fix-20260823T214751Z-79cfb27f branch=task/bug-MDR-BUG-FLU-00078-run-fix-20260823T214751Z-79cfb27f code=bcfc91f gate=manual)

## Observation

The integrated LocalSystem service starts, but every console-worker launch fails at SetTokenInformation(TokenSessionId) with access denied, so no control, capture, audio, or input listener appears. Rhydra requested a hand-selected duplicated-token access mask; Sunshine’s proven service path requests TOKEN_ALL_ACCESS for the duplicated LocalSystem primary token. Restore that exact access contract and verify the worker launches on Quench.

## Fix

`bcfc91f5f7a0328665b1cdf700732359ea9db3e0` changes `launch_worker` to request
`TOKEN_ALL_ACCESS` from `DuplicateTokenEx` before assigning the active console
session to the duplicated primary token.

## Notes

## Verification

The source verification tree was commit `3dfda67d204045ff8d196716a8001730ffd83c03`,
which contains the fix commit `bcfc91f5f7a0328665b1cdf700732359ea9db3e0`. The
production call is at `tools/latency-spike/server/src/win/service.rs:214-222`.

The lead source oracle passed with `TOKEN_ALL_ACCESS`. Replacing it with the exact
pre-fix mask (`TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_QUERY |
TOKEN_ADJUST_SESSIONID`) failed the oracle, and restoring it passed again. An
independent verifier reproduced the same red mutant and green restoration.

The six repository gates all exited 0 on the verification tree: `cargo build
--locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy
--all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and
`./scripts/check-windows.sh --locked`. The Windows gate compiled the service code;
it does not execute the Windows service.

The required Quench runtime regression could not be rerun. `quench.lan.example` did
not resolve from this host, and SSH to the recorded address `192.0.2.240` timed
out. No SCM, LocalSystem token, `SetTokenInformation`, control listener, capture,
audio, or input launch evidence is claimed. The verification claim was cleared
by `1b7f11d291c47e6a539fa7c6245f7724a61dec93`; the record remains Fixed
with one indeterminate attempt pending a reachable Quench runtime check.
