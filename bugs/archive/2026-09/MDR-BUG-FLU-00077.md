# MDR-BUG-FLU-00077 — Service ACL hardening strips access from deployed executables

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** Rhydra deploy
- **Raised:** 2026-08-23T21:42:09Z
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
- **State history:** Open (2026-08-23T21:42:09Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T21:44:21Z, deltic:auto role=fix run=fix-20260823T214409Z-8a9b6f88 branch=task/bug-MDR-BUG-FLU-00077-run-fix-20260823T214409Z-8a9b6f88 code=b8c04fc gate=manual) -> Closed (2026-09-13T12:44:18Z, 0x4D44/Codex verify run=verify-20260913T123100Z-6776825d)

## Observation

On the first integrated LocalSystem-service deploy, harden_installation_acl combined recursive /inheritance:r with inheritable root grants. Windows removed inherited ACEs from every descendant after propagation, leaving rhydra-agent.exe with an empty ACL; SCM registered the service but StartService failed with access denied. The install sequence must protect and grant the root first, then explicitly re-enable inheritance below it, with a regression test for command ordering.

## Fix

The installer protects the deployment root first, resets the root ACL, applies the SYSTEM and Administrators grants without inheritance, then resets only descendants recursively so deployed executables inherit the protected root permissions.

## Notes

## Verification

The verification build is commit 36b4504f900806239ad1be009be8d6a60e9e105e and contains the full fix commit b8c04fc605075f431529f171ace02bdbddfc86c2. The command plan is built in `tools/latency-spike/server/src/bin/agent.rs:64-90`, and Windows execution applies it step by step in `tools/latency-spike/server/src/bin/agent.rs:729-743`.

The lead focused commands `tests::acl_plan_protects_the_root_then_reenables_descendant_inheritance` and `tests::service_command_quotes_the_exact_versioned_executable` each passed: 1 passed, 0 failed, 3 filtered out. An independent verifier ran the ACL regression and all four `rhydra-agent` tests; they passed.

As a lead root behavioral mutant, I changed the final descendant reset target from `root\\*` to `root`. The ACL regression failed with exit 101 at `tools/latency-spike/server/src/bin/agent.rs:229`, observing `C:\\mdrdp` where the expected descendant target was `C:\\mdrdp\\*`. The independent verifier restored the old recursive `/inheritance:r ... /T` command; its regression failed because the command plan had 1 entry instead of 4. Restoring the source left an empty diff, and the positive regression passed 1/1 again.

The six repository gates all exited 0 on this tree: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate provides compilation evidence only; `icacls.exe`, SCM registration, and `StartService` were not run on this macOS host.
