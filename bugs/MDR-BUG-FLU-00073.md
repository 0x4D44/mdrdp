# MDR-BUG-FLU-00073 — Native input writes can block forever when the host stops reading

- **State:** Fixed
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
- **State history:** Open (2026-08-23T20:34:25Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-23T21:28:41Z, deltic:auto role=fix run=fix-20260823T211540Z-b4924d4a branch=task/bug-MDR-BUG-FLU-00073-run-fix-20260823T211540Z-b4924d4a code=d94c3dc6ee7971a3a753c11adbe150a5f83139a8 gate=manual)

## Observation

The native input pump drains its reliable queue through blocking TcpStream::write_all, while the input socket has no write timeout or non-blocking handoff. If the host accepts the channel but stops reading, native-input parks indefinitely and every later key or button waits behind it. Bound the write or move it behind a bounded writer queue, and prove teardown and later input cannot wedge.

## Fix

<unfixed — raised only>

## Notes
