# MDR-BUG-FLU-00038 — A partial Rhydra input record blocks every later input client indefinitely

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/input
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T13:31:35Z, deltic:auto role=fix run=fix-20260823T131759Z-56a2bc45 branch=task/bug-MDR-BUG-FLU-00038-run-fix-20260823T131759Z-56a2bc45 code=34011e7 gate=manual) -> Closed (2026-09-13T08:57:12Z, 0x4D44/Codex verify run=verify-20260913T084817Z-f8be8942)

## Observation

tools/latency-spike/server/src/win/input.rs:453-505 reads one input record with unbounded read_exact calls, and input.rs:581-587 serves one connection inline. A peer that sends only a kind byte or partial body and remains connected blocks the sole listener from accepting a replacement input connection. Bound partial-record reads and recover the listener.

## Fix

Input record parsing now leaves the socket unlimited while it waits for the next
kind byte, then applies one absolute one-second deadline to the rest of that record.
A silent or byte-dribbling partial record therefore ends only that connection;
`serve_one` still flushes movement telemetry and releases held keys/buttons before
the listener accepts its replacement.

The socket-boundary logic is portable and tested on loopback. Regression tests
cover a silent partial body, a byte dribble which cannot extend the deadline,
healthy idle time between complete records, and the existing unknown-kind close.

## Notes

## Verification

Independent verification confirmed fix commit 34011e7291ced4d3bf4ad8c8bfb6ec9067441456. The current parser starts one absolute body deadline in tools/latency-spike/server/src/input_stream.rs:32, applies the remaining timeout to each read, and returns through the input connection cleanup at tools/latency-spike/server/src/win/input.rs:560-603.

The focused input_stream test suite ran 4 tests with 0 failures and 347 filtered. It covers silent partial bodies, byte dribbles that cannot extend the deadline, idle gaps between complete records, and unknown-kind closure. As a red root mutant, moving the deadline calculation inside the body-read loop renewed the timeout for every byte; a_byte_dribble_cannot_extend_the_body_deadline then failed at tools/latency-spike/server/src/input_stream.rs:111 because unwrap_err received Complete(8). The absolute-deadline source was restored and all 4 focused tests passed again.

The repository gates then passed: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows checks exited 0; they emitted only the repository's existing icon and src/present.rs unused-parameter warnings.

No live Windows Rhydra session was started because it would require remote access and host mutation. The original partial-record observation remains the end-to-end product evidence; this pass verifies the portable deadline and listener-recovery path locally.
