# MDR-BUG-FLU-00044 — An unterminated Rhydra agent control request can grow memory without bound

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/control
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T20:01:26Z, deltic:auto role=fix run=fix-20260823T195542Z-8f67a99b branch=task/bug-MDR-BUG-FLU-00044-run-fix-20260823T195542Z-8f67a99b code=a071f0d gate=manual) -> Closed (2026-09-13T09:34:16Z, 0x4D44/Codex verify run=verify-20260913T092549Z-f6c00b3c)

## Observation

tools/latency-spike/server/src/bin/agent.rs:359-379 reads control requests with BufReader::lines and has a per-read timeout but no maximum line length. A local peer can continuously send a newline-free request slowly enough to avoid the timeout while growing the allocation until the agent exhausts memory. Enforce a small protocol line ceiling before allocation grows.

## Fix

The portable control protocol now reads through a `Take`-bounded `BufRead` view and
rejects a line immediately after one byte beyond `MAX_REQUEST_LINE_BYTES`. The
Windows agent uses that reader instead of `BufRead::lines`, preserving LF/CRLF and
final unterminated-line semantics while placing a hard ceiling on allocation.

The ceiling allows the auxiliary channel's largest legal clipboard expectation even
after worst-case JSON escaping, plus its request envelope. Regression tests prove an
unterminated stream is consumed only through limit + 1, ordinary and EOF-final lines
retain their content, and the maximum supported clipboard comparison still parses.

## Notes

## Verification

Independent verification confirmed fix commit a071f0dd1a43329545fb2ed477e63c069c6d75c5. The Windows agent now calls the bounded reader from tools/latency-spike/server/src/bin/agent.rs:510-513, and tools/latency-spike/server/src/control.rs:36-57 uses `Take(MAX_REQUEST_LINE_BYTES + 1)` so an unterminated request can allocate and consume only one byte beyond the ceiling before rejection.

The lead focused `control::tests` run passed 41/41, covering the byte ceiling, unterminated input, complete and EOF-final lines, and the largest legal clipboard JSON expectation. The independent verifier reproduced the same 41/41 result and observed the unterminated test consuming exactly 1,573,121 bytes (limit + 1).

As a red root mutant, changing the bounded reader at tools/latency-spike/server/src/control.rs:39 from `MAX_REQUEST_LINE_BYTES as u64 + 1` to `MAX_REQUEST_LINE_BYTES as u64` made `an_unterminated_control_line_is_rejected_at_the_byte_ceiling` fail because the oversized input was returned as `Some(...)` instead of an error. The source was restored and all 41 control tests passed again. The independent verifier separately removed the bound and observed the cursor advance beyond the expected limit.

The six repository gates passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows check exited 0 and emitted only the existing unused `width`/`height` warnings in src/present.rs. No live Windows control agent was exercised on this macOS host, so runtime socket behavior remains unverified.
