# MDR-BUG-FLU-00040 — Rhydra orphan cleanup can kill unrelated same-named processes

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/process-lifecycle
- **Raised:** 2026-08-22T19:40:40Z
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T20:08:11Z, deltic:auto role=fix run=fix-20260823T200141Z-40b1bfac branch=task/bug-MDR-BUG-FLU-00040-run-fix-20260823T200141Z-40b1bfac code=2b8a54a gate=manual) -> Closed (2026-09-13T09:10:22Z, 0x4D44/Codex verify run=verify-20260913T085841Z-c6293da0)

## Observation

tools/latency-spike/server/src/win/agent_ops.rs:321-329 and src/bin/agent.rs:638-655 invoke taskkill by image name without PID, executable path, session, or ownership filtering. Another user or test stack running a same-named server or creator can be forcibly terminated. Cleanup must target only processes owned by this installation.

## Fix

<unfixed — raised only>

## Notes

## Verification

Independent verification confirmed fix commit 2b8a54a9798af64b53c7aa0e886c5adb6ee15326. The current cleanup path in tools/latency-spike/server/src/win/agent_ops.rs:319-416 filters candidate images, opens each PID, resolves its full executable path, compares it with the installation path, and terminates only the same opened process handle. Startup and uninstall both call this ownership-scoped helper, and no taskkill references remain under tools/latency-spike/server/src.

The focused process_ownership test module passed 2 tests with 0 failures and 349 filtered. It covers rejection of the same executable name in another directory and acceptance of Windows case/separator spelling differences.

As a red root mutant, replacing the normalized full-path comparison with true made process_ownership::tests::same_name_in_another_directory_is_not_owned fail at tools/latency-spike/server/src/process_ownership.rs:28. The comparison was restored and both focused ownership tests passed again.

The repository gates then passed: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows checks exited 0; they emitted only the repository's existing icon and src/present.rs unused-parameter warnings.

No live Windows process cleanup was run because it would require a Windows host and destructive process operations. The Windows gate is compile-only, so runtime PID/path filtering remains unverified. The original same-name process observation remains the end-to-end product evidence; this pass verifies the portable ownership predicate and its caller wiring locally.
