# MDR-BUG-FLU-00039 — Silent Rhydra auxiliary peers can hold the only connection forever

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/aux
- **Raised:** 2026-08-22T19:40:40Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T085831Z-67a74c23
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00039-run-verify-20260913T085831Z-67a74c23
- **Owner base:** c8305d66924fa048e9a936b384e4696e4cc66666
- **Owner fingerprint:** sha256:992a3e924b523b86503db0213726f71066390680aedddbb50b339d08301a5c70
- **Owner since:** 2026-09-13T08:58:31Z
- **Owner until:** 2026-09-13T10:58:31Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T19:55:19Z, deltic:auto role=fix run=fix-20260823T194649Z-c3161b01 branch=task/bug-MDR-BUG-FLU-00039-run-fix-20260823T194649Z-c3161b01 code=edc5742 gate=manual)

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
