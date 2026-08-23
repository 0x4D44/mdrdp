# MDR-BUG-FLU-00073 — Native input writes can block forever when the host stops reading

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native/input-latency
- **Raised:** 2026-08-23T20:34:25Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260823T211540Z-b4924d4a
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00073-run-fix-20260823T211540Z-b4924d4a
- **Owner base:** 5178f7633a528f56a9ae28c9a6041ff8e0c00ca8
- **Owner fingerprint:** -
- **Owner since:** 2026-08-23T21:15:40Z
- **Owner until:** 2026-08-23T23:15:40Z
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
