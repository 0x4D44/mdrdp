# MDR-BUG-FLU-00039 — Silent Rhydra auxiliary peers can hold the only connection forever

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/aux
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T19:55:19Z, deltic:auto role=fix run=fix-20260823T194649Z-c3161b01 branch=task/bug-MDR-BUG-FLU-00039-run-fix-20260823T194649Z-c3161b01 code=edc5742 gate=manual) -> Closed (2026-09-13T09:10:22Z, 0x4D44/Codex verify run=verify-20260913T085831Z-67a74c23)

## Observation

tools/latency-spike/server/src/aux_server.rs:169-176 configures only TCP_NODELAY. The reader and writer at auxchan.rs:399-410 and auxchan.rs:330-340 have no deadlines, so a silent or partial peer can block the serial auxiliary server and a non-reading peer can strand its writer. Add bounded I/O and reconnect behavior.

## Fix

The auxiliary accept loop now remains available while one connection is served on
an owned session thread. A newly accepted connection shuts down and joins the old
owner before creating clipboard handles and starting, so the newest viewer can evict
a silent peer without ever letting two clients race the host clipboard.

Both host and client auxiliary sockets now bound writes at five seconds. A writer
failure shuts down the socket and therefore wakes the paired reader, allowing the
host session thread to tear down its poll and audio workers. The reconnect regression
uses real loopback sockets: a first client sends nothing, a second replaces it, and
only the second client's clipboard payload reaches the fake host pasteboard.

## Notes

## Verification

Independent verification confirmed fix commit edc5742250fd87899d15da42b11c76e65d1d2aed. The current auxiliary accept path in tools/latency-spike/server/src/aux_server.rs:125-207 keeps the listener accepting while one connection owns a session thread, shuts down and joins the old owner, and starts the replacement before it can touch the host clipboard. The shared five-second write bound is present in tools/latency-spike/server/src/auxchan.rs:40 and is applied at the host and native client socket setup sites.

The focused regression aux_server::tests::a_new_client_replaces_a_silent_connection_owner passed with 1 test and 0 failures; its second loopback client delivered new owner to the fake host pasteboard. The supporting regression aux_server::tests::a_stuck_clipboard_write_does_not_stop_the_poll_loop also passed with 1 test and 0 failures.

As a red root mutant, returning Ok before replace_active starts the connection owner made a_new_client_replaces_a_silent_connection_owner fail at tools/latency-spike/server/src/aux_server.rs:655 after the replacement wait expired: timed out waiting for the replacement client to own the clipboard. The replacement path was restored and the focused regression passed again. No dedicated auxiliary socket write-timeout regression exists; that five-second bound was source-inspected.

The repository gates then passed: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows checks exited 0; they emitted only the repository's existing icon and src/present.rs unused-parameter warnings.

No live Windows Rhydra auxiliary session was started because it would require remote access and host mutation. The Windows gate is compile-only, so live socket eviction and write-timeout behavior remain unverified. The original silent-peer observation remains the end-to-end product evidence; this pass verifies the portable replacement lifecycle locally.
