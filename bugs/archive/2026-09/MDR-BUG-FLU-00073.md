# MDR-BUG-FLU-00073 — Native input writes can block forever when the host stops reading

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native/input-latency
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
- **State history:** Open (2026-08-23T20:34:25Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:28:41Z, deltic:auto role=fix run=fix-20260823T211540Z-b4924d4a branch=task/bug-MDR-BUG-FLU-00073-run-fix-20260823T211540Z-b4924d4a code=d94c3dc6ee7971a3a753c11adbe150a5f83139a8 gate=manual) -> Closed (2026-09-13T12:13:00Z, 0x4D44/Codex verify run=verify-20260913T120236Z-2657d1fc)

## Observation

The native input pump drains its reliable queue through blocking TcpStream::write_all, while the input socket has no write timeout or non-blocking handoff. If the host accepts the channel but stops reading, native-input parks indefinitely and every later key or button waits behind it. Bound the write or move it behind a bounded writer queue, and prove teardown and later input cannot wedge.

## Fix

Native input writes use a bounded timeout and an absolute deadline for each wire record. A host that stops reading therefore fails the native input pump and releases later input instead of wedging the session indefinitely.

## Notes

## Verification

The verification build is commit 93c319f1c189c568f0b8abbec011ec63de0b498f and contains the full fix commit d94c3dc6ee7971a3a753c11adbe150a5f83139a8. Native input configures the bounded writer in src/native/session.rs:1392-1407, and write_record_until reapplies the remaining deadline at line 1633 while handling partial writes.

The lead focused commands native::session::tests::a_nonreading_input_peer_times_out_instead_of_wedging_native_input, native::session::tests::dribbling_native_record_honours_one_absolute_deadline, and native::session::tests::native_reliable_input_batch_yields_to_commands each passed: 1 passed, 0 failed, 907 filtered out. An independent verifier reran the nonreading-peer regression: 1 passed, 0 failed, and also passed cargo fmt --all -- --check, clippy with -D warnings, and the Windows type-check.

As a lead root behavioral mutant, I changed the helper's remaining-deadline calculation in src/native/session.rs:1623 to a fixed five-second duration. The deterministic dribbling regression failed with exit 101 because the helper returned Ok instead of the expected timeout. The independent verifier changed writer.set_write_timeout(Some(remaining)) to None at src/native/session.rs:1633; the nonreading-peer test failed at its timeout assertion because the writer did not terminate. Restoring the source left an empty diff and the selected positive regression passed 1/1 again.

The six repository gates all exited 0 on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate emitted the existing unused width/height warnings in src/present.rs.

No live RDP server, Windows runtime, or network session was exercised, so this closure relies on the loopback writer tests and source-level mutant evidence.
