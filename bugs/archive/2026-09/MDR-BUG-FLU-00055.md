# MDR-BUG-FLU-00055 — Native input latency telemetry starts after the whole queued burst

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rhydra/telemetry
- **Raised:** 2026-08-23T12:09:38Z
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
- **State history:** Open (2026-08-23T12:09:38Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T22:22:10Z, deltic:auto role=fix run=fix-20260823T221118Z-3786a30b branch=task/bug-MDR-BUG-FLU-00055-run-fix-20260823T221118Z-3786a30b code=9d88631 gate=manual) -> Closed (2026-09-13T10:53:43Z, 0x4D44/Codex verify run=verify-20260913T104534Z-9be4180c)

## Observation

The native input pump drains and writes the available record burst, then stamps InputClock once. The metric claims to measure the first unanswered input but starts after the final write, understating the first event by the rest of the burst. Stamp when the first record is committed to the socket and preserve one outstanding causal sample until paint.

## Fix

The native input path stamps InputClock immediately after the first successful record write and carries one unanswered sample across the rest of the burst. Later records and events cannot replace that first causal timestamp.

## Notes

## Verification

The verification build contains fix commit 9d88631aac7c56d213f3bd8fd5da8f75ff7bf217. The focused native test native::session::tests::native_input_stamps_before_the_second_record_of_a_burst passed 1/1. An independent verifier also ran the paint-close, first-unanswered-sample, and input-thread stamping tests; each selected one test and passed.

As lead root mutant, the first-record stamp branch was disabled. The selected test failed with exit 101 at session.rs:5811: the first record must start the clock before the next write. The independent verifier instead moved the stamp after the complete record loop; the same selected test failed on that assertion. Restoring the exact source made the test pass 1/1 and left an empty source diff.

The six repository gates passed on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate exited 0 with the existing unused width/height warnings.

No live rhydra host, production socket timing, benchmark, or latency distribution was collected or claimed.
