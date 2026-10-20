# MDR-BUG-FLU-00049 — Native probe connect retries can exceed and misreport their deadline

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** native/startup
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T18:46:12Z, deltic:auto role=fix run=fix-20260824T183819Z-f4fd4740 branch=task/bug-MDR-BUG-FLU-00049-run-fix-20260824T183819Z-f4fd4740 code=61070bb gate=manual) -> Closed (2026-09-13T10:12:07Z, 0x4D44/Codex verify run=verify-20260913T100235Z-9eca0c7f)

## Observation

The native SSH readiness and input-probe loops grant each attempt a fixed 250 ms connect timeout and then sleep 50 ms without bounding either by the remaining deadline. A final attempt can overshoot the advertised budget and the input path can report a generic I/O failure instead of Deadline. Derive each connect and sleep duration from the remaining budget and test the deadline boundary.

## Fix

Native probe channel retries now derive every connect timeout and retry sleep from the remaining absolute probe deadline. When that deadline expires, the input, sparse, and tunnel-readiness paths report `ProbeFailure::Deadline` instead of overshooting the budget or returning a generic refusal.

## Notes

## Verification

Independent verification confirmed fix commit 61070bb13350d49b7ef8b367d9b397ced8bc6de8. Required input and sparse channels share the bounded loop in src/native/probe.rs:466-513; tunnel readiness uses the same helper in src/native/ssh.rs:413-456. The probe test at src/native/probe.rs:1015 and readiness test at src/native/ssh.rs:604 exercise the deadline classification and bounded retry sleep.

The lead input-probe and tunnel-readiness tests each passed 1/1. The independent verifier reproduced both tests at 1/1 and restored the old fixed 250 ms connect and 50 ms sleep budgets in a disposable copy; both tests then failed their elapsed-deadline assertions. No live host, SSH, or network runtime evidence was collected.

As a red root mutant, changing `remaining.min(cap)` to an unconditional `cap` in src/native/ssh.rs:450 made `input_retry_reports_and_honours_the_probe_deadline` fail with `input retry exceeded the probe deadline`. The source was restored; both focused tests passed again. The independent verifier also confirmed the restoration by rerunning both tests.

The six repository gates passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate exited 0 and emitted only the existing missing icon asset and unused `width`/`height` warnings in src/present.rs.

The refusal fixture returns immediately, so these tests prove retry-sleep bounding and deadline classification directly; they do not force a blocked `connect_timeout`.
