# MDR-BUG-FLU-00046 — A 256-event RDP input burst terminates the session

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** session/input
- **Raised:** 2026-08-23T11:57:47Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T093557Z-549a5f29
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00046-run-verify-20260913T093557Z-549a5f29
- **Owner base:** d7bde76a646712446647f9b5698a35919bdcab29
- **Owner fingerprint:** sha256:1f7930c6cec58090c5063e750c2026953ba0af8437dc361155593a6a45c52b21
- **Owner since:** 2026-09-13T09:35:57Z
- **Owner until:** 2026-09-13T11:35:57Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T11:57:47Z, raised via `deltic bugs new`) -> Fixed (2026-08-23T12:07:12Z, deltic:auto role=fix run=fix-20260823T115827Z-c9dcbf14 branch=task/bug-MDR-BUG-FLU-00046-run-fix-20260823T115827Z-c9dcbf14 code=59fcb85 gate=manual) -> Closed (2026-09-13T09:48:16Z, 0x4D44/Codex verify run=verify-20260913T093557Z-549a5f29)

## Observation

The RDP pump drains every queued input event into one fast-path PDU. Fast-path has an 8-bit event count and rejects 256 or more events, so a normal accumulated burst returns a protocol error and ends the live session. Expected: input is sent in bounded valid batches and the pump yields to inbound work while excess events remain queued.

## Fix

The session pump now limits each fast-path input PDU to 255 events, the largest count representable by its one-byte event-count field. A full batch returns immediately with a zero wait so inbound work gets a turn, while excess input remains queued for the next pump turn.

## Notes

## Verification

Independent verification confirmed fix commit 59fcb850f4e72dbaed778e043761e9c185720fef. The bound is `FASTPATH_INPUT_BATCH_MAX = 255` at src/session.rs:43-45; `drain_input` enforces it at :1334-1385, and `readiness_wait_after_work` yields immediately after a full batch at :1405-1410. The regression test queues 256 events and proves one remains queued after a valid send at :2286-2322.

The lead ordinary-batch test and 256-event split test each passed 1/1. The independent verifier ran the full session test module (96/96) and input test module (49/49). No live RDP server run was available on this macOS host.

As a red root mutant, changing `FASTPATH_INPUT_BATCH_MAX` from 255 to 256 made `an_oversized_input_burst_is_split_across_pump_turns` fail at src/session.rs:2310 with `Protocol("encode input: fast-path batch must hold 1..=255 events, got 256")`. The independent verifier separately removed the batching bound and observed the same test fail. The constant was restored, and both focused tests passed again.

The six repository gates passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate exited 0 and emitted only the existing unused `width` and `height` warnings in src/present.rs.
