# MDR-BUG-FLU-00075 — RDP clipboard work can block graphics decode and input on the sole session thread

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rdp/clipboard-latency
- **Raised:** 2026-08-23T20:34:25Z
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
- **State history:** Open (2026-08-23T20:34:25Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T07:57:10Z, deltic:auto role=fix run=fix-20260824T065233Z-095a1cce branch=task/bug-MDR-BUG-FLU-00075-run-fix-20260824T065233Z-095a1cce code=3e5b56f1191abdfedce1b11c0f8f185e859b8766 gate=manual) -> Closed (2026-09-13T12:28:03Z, 0x4D44/Codex verify run=verify-20260913T121351Z-af272b56)

## Observation

The RDP session services an unbounded clipboard action drain before reading graphics, performs OS clipboard and image conversion synchronously, and writes clipboard responses through a blocking socket with no write timeout. A slow pasteboard, large image, or peer that stops reading can delay graphics decode and all input indefinitely. Bound clipboard work per turn and isolate or bound its blocking I/O without breaking static-channel ordering.

## Fix

Clipboard OS access and conversion run in one serialized worker with one-deep command and result queues. The session pump processes bounded actions and results, rings the session on completion, retains reliable work under backpressure, and bounds shutdown and transport writes.

## Notes

## Verification

The verification build is commit 3802fdaac1fa79c1a4d048be7f677f46ec4888dc and contains the behavioral fix commit 48ede2f4e694ec914d0db468e830f665b51f2175. The Fixed history names 3e5b56f1191abdfedce1b11c0f8f185e859b8766, which is a version-only transition; the code and fix history identify 48ede2f4e694ec914d0db468e830f665b51f2175 as the clipboard fix. The production constructor uses the worker in `src/clipboard.rs:733-768`, and `pump_bounded` limits session-thread clipboard work in `src/clipboard.rs:837-892`.

The lead focused commands `clipboard::tests::production_pump_does_not_wait_for_a_blocked_os_read`, `clipboard::tests::pump_limits_work_and_continues_reliable_actions_in_fifo_order`, `clipboard::tests::a_second_poll_during_a_slow_read_does_not_lose_the_change`, and `clipboard::tests::saturated_local_data_request_gets_an_immediate_error_response` each passed: 1 passed, 0 failed, 907 filtered out. An independent verifier reran the blocked-OS-read regression and passed 1 test.

As a lead root behavioral mutant, I changed `clipboard_channel` at `src/clipboard.rs:737` from the production worker constructor to the old inline executor. The selected regression failed with exit 101 at `src/clipboard.rs:2025` because the production pump waited for the blocked OS clipboard read. The independent verifier reproduced the same failure. Restoring the source left `git diff --exit-code -- src/clipboard.rs` empty, and the positive regression passed 1/1 again.

The six repository gates all exited 0 on this tree: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate emitted the existing unused width/height warnings in `src/present.rs` and the existing icon-tool warning. Independent hygiene also passed with `git diff --check`.

No live RDP server, GUI compositor, physical display, or real system clipboard was exercised, so this closure relies on the clipboard worker tests and source-level mutant evidence.
