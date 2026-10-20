# MDR-BUG-FLU-00037 — Rhydra advertises input and auxiliary ports even when their listeners fail to bind

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/server-lifecycle
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
- **State history:** Open (2026-08-22T19:40:40Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T13:38:12Z, deltic:auto role=fix run=fix-20260823T133202Z-ba991e4f branch=task/bug-MDR-BUG-FLU-00037-run-fix-20260823T133202Z-ba991e4f code=e21cada gate=manual) -> Closed (2026-09-13T08:57:12Z, 0x4D44/Codex verify run=verify-20260913T084808Z-d39b68cb)

## Observation

In tools/latency-spike/server/src/win/pipeline.rs:231-271, input and auxiliary listener startup failures are only logged after the video header has already advertised those ports at pipeline.rs:384-390. The video session can therefore look connected while input or auxiliary services are dead. Listener readiness must gate advertisement or terminate the server before a client receives an unusable contract.

## Fix

Session startup now binds the input listener and the optional auxiliary listener
as one synchronous transaction before constructing the video header or listener.
The already-bound sockets move into their service threads, so a client can never
receive an advertised side-channel contract whose bind failed. If the auxiliary
bind fails, the input listener is dropped as the startup error unwinds.

The auxiliary-disabled path still binds input alone and advertises no clipboard.
Portable regression tests cover both successful shapes and all-or-nothing cleanup
when the second bind is occupied.

## Notes

## Verification

Independent verification confirmed that 00037 is distinct from 00041: this record covers side-channel listener binding and advertisement, while 00041 covers HEVC level selection. The exact fix is commit e21cadacaadb032a9991abf5c4dd5794ca433d9a. The current transaction binds input, sparse, and optional auxiliary listeners in tools/latency-spike/server/src/channel_listeners.rs:13-22 before the Windows pipeline advertises its ports.

The focused server regression suite channel_listeners ran 2 tests with 0 failures and 349 filtered. The tests cover successful required-channel shapes and release the input bind when the auxiliary bind fails. As a red root mutant, auxiliary bind failure was swallowed by falling back to an ephemeral listener; an_aux_bind_failure_releases_the_input_bind_too then failed at tools/latency-spike/server/src/channel_listeners.rs:54 because BoundChannels::bind returned Ok. The original bind error path was restored and the focused suite passed again.

The repository gates then passed: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows checks exited 0; they emitted only the repository's existing icon and src/present.rs unused-parameter warnings.

No live Rhydra Windows listener or client session was run because the available local host is macOS and a live run would require host access. The Windows gate is compile-only, so live pipeline startup and video-header behavior remain outside this pass. The original bind-failure observation remains the end-to-end product evidence; this pass verifies the portable transaction and its failure behavior.
