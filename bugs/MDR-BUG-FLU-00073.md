# MDR-BUG-FLU-00073 — Native input writes can block forever when the host stops reading

- **State:** Open
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
- **State history:** Open (2026-08-23T20:34:25Z, raised via `deltic bugs new` model=gpt-5.6-sol@high)

## Observation

The native input pump drains its reliable queue through blocking TcpStream::write_all, while the input socket has no write timeout or non-blocking handoff. If the host accepts the channel but stops reading, native-input parks indefinitely and every later key or button waits behind it. Bound the write or move it behind a bounded writer queue, and prove teardown and later input cannot wedge.

## Fix

<unfixed — raised only>

## Notes
