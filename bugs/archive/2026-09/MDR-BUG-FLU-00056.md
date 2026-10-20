# MDR-BUG-FLU-00056 — Spike viewer input writes can block its window thread indefinitely

- **State:** Closed
- **Priority:** Should
- **Severity:** High
- **Area:** rhydra/measurement-viewer
- **Raised:** 2026-08-23T12:09:38Z
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
- **State history:** Open (2026-08-23T12:09:38Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T08:32:06Z, deltic:auto role=fix run=fix-20260824T081954Z-3084288e branch=task/bug-MDR-BUG-FLU-00056-run-fix-20260824T081954Z-3084288e code=a7c63298e0afb3c771009092b7e9bbed6bcb080f gate=manual) -> Closed (2026-09-13T10:53:43Z, 0x4D44/Codex verify run=verify-20260913T104546Z-5c2feb30)

## Observation

The latency-spike viewer calls blocking write_all directly from its winit key handler and sets no write timeout. If the peer accepts but stops reading, repeated key events can fill the socket buffer and freeze input and presentation. Move writes off the window thread or enforce a bounded non-blocking handoff.

## Fix

The spike viewer keeps healthy input sends on the direct path but bounds each socket write to 10 ms. A short or failed write closes the frameless link, and later records are rejected immediately so the window thread cannot remain blocked.

## Notes

## Verification

The Fixed history records a7c63298e0afb3c771009092b7e9bbed6bcb080f, which is a version-only commit. Its parent 19531f29114e06abc90cfada24017a7000f0e8c1 is the implementation commit: it adds the 10 ms write deadline, fails closed on partial or timed-out writes, and disables forwarding after the first error. Both commits are ancestors of the verification build.

The focused viewer test input_link::tests::a_nonreading_peer_cannot_wedge_the_input_link passed 1/1. It exercises a non-reading peer, bounded backpressure failure, immediate rejection of later records, and a timely caller return.

As a root behavioral mutant, the write timeout setup was changed from the 10 ms deadline to no timeout. The same selected test failed with exit 101 at input_link.rs:152: socket backpressure exceeded the viewer’s bounded failure budget. Restoring the exact source made the test pass 1/1 and left an empty source diff. An independent verifier reproduced the same assertion and confirmed the restored source; it also confirmed app.rs drops input forwarding after failure.

The six repository gates passed on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate exited 0 with the existing unused width/height warnings.

No live viewer, GUI freeze, network, benchmark, or production timing evidence was collected or claimed.
