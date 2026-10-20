# MDR-BUG-FLU-00099 — Partial RDP PDU can monopolize the session thread

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** rdp/session-latency
- **Raised:** 2026-08-24T11:29:10Z
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
- **State history:** Open (2026-08-24T11:29:10Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T11:48:59Z, deltic:auto role=fix run=fix-20260824T112930Z-bc42a9c8 branch=task/bug-MDR-BUG-FLU-00099-run-fix-20260824T112930Z-bc42a9c8 code=a2bd5af gate=manual) -> Closed (2026-09-14T07:42:20Z, 0x4D44/Codex verify run=verify-20260914T071757Z-203bddf6)

## Observation

The session pump calls ironrdp-blocking Framed::read_pdu, which loops until a complete PDU. The socket timeout is per read, so a peer trickling bytes within each 5 ms slice can keep the sole session thread inside one call indefinitely, delaying queued input and preventing shutdown. Reads must return to the pump after currently available bytes while preserving TLS and framed partial state.

## Fix

`src/session.rs` temporarily switches the framed stream to nonblocking mode, lets rustls and the RDP framer preserve incomplete state, then restores blocking mode before writes (`66c2dd0`, integrated by `a2bd5af`).

## Verification

Independent verifier ran `partial_pdu_read_returns_would_block_and_resumes_from_framer_state` and `partial_hint_read_returns_would_block_and_restores_mode`; both passed, and the 96-test session family passed. Changing the nonblocking mode argument from `true` to `false` made the partial-PDU regression fail at `src/session.rs:1492` with `TimedOut` instead of `WouldBlock`; restoration returned the focused and session tests to green.

Lead verifier ran both partial-read regressions and `the_session_socket_bounds_both_read_and_write_stalls`; all passed. Keeping framed reads blocking made `partial_pdu_read_returns_would_block_and_resumes_from_framer_state` fail at `src/session.rs:1493` with `TimedOut` instead of `WouldBlock`; restoration passed all six batch-focused tests.

All repository gates passed on tree `77bddd164ea101cd32f65a06ac95f97ddd57bc20`: `cargo build --locked`, `cargo test --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh`, and `./scripts/check-windows.sh --locked`. Windows checking emitted only the existing unused `width` and `height` warnings at `src/present.rs:95`. No live RDP interoperability session was run; deterministic framed-stream tests exercise partial-read recovery and state preservation.
