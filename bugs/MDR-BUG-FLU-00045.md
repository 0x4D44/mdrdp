# MDR-BUG-FLU-00045 — Rhydra send_done telemetry records failed video delivery as success

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rhydra/telemetry
- **Raised:** 2026-08-22T19:40:41Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T093547Z-dbf682aa
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00045-run-verify-20260913T093547Z-dbf682aa
- **Owner base:** beae2a547d911687ded9309fd6a19e622c5bcefa
- **Owner fingerprint:** sha256:97695cacd767afc21b3d004c92d8918f6c377fe7efbacc3b623e50a9b1059038
- **Owner since:** 2026-09-13T09:35:47Z
- **Owner until:** 2026-09-13T11:35:47Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:41Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-25T06:09:45Z, deltic:auto role=fix run=fix-20260825T055844Z-0953b581 branch=task/bug-MDR-BUG-FLU-00045-run-fix-20260825T055844Z-0953b581 code=eb8e10c3e0e32c03eea73613ac90f637868ffd51 gate=manual) -> Closed (2026-09-13T09:48:16Z, 0x4D44/Codex verify run=verify-20260913T093547Z-dbf682aa)

## Observation

tools/latency-spike/server/src/win/send.rs:159-177 swallows socket write or flush errors after dropping the client, and send.rs:218-232 stamps send_done_us unconditionally. Stats therefore report wire completion and latency for access units that never reached a client. Propagate delivery outcome into the telemetry record.

## Fix

Rhydra now propagates the result of the payload write and flush. The sender returns failure when no client exists or either socket operation fails, drops a failed client, and records `send_done_us` only when `emit_if_delivered` receives success. Video and frame-set telemetry rows therefore represent payloads delivered to the client.

## Notes

## Verification

Independent verification confirmed fix commit eb8e10c3e0e32c03eea73613ac90f637868ffd51. The delivery result flows from tools/latency-spike/server/src/win/send.rs:381-399 and :415-417 through the Video and FrameSet handlers at :454-483 into tools/latency-spike/server/src/send_schedule.rs:61-72. The overlap warning with MDR-BUG-FLU-00050 was resolved: 00050 covers payload-first ordering and periodic stats-file flushing, while this record covers whether a payload write and flush succeeded before telemetry is stamped.

The lead focused `send_schedule::tests` run passed 4/4. The independent verifier reproduced 4/4 and confirmed both mdrdp and standalone rhydra with the Windows cross-target check. No live Windows socket or viewer run was available on this macOS host.

As a red root mutant, changing `if delivered` to `if true` at tools/latency-spike/server/src/send_schedule.rs:68 made `delivery_telemetry_requires_successful_write_and_flush` fail with `left: 42`, `right: 0` at :142. The independent verifier reproduced the same failure in an isolated copy. The source was restored and the focused four-test module passed again.

The six repository gates passed: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate exited 0 and emitted only the existing unused `width` and `height` warnings in src/present.rs.
