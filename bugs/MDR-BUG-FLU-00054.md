# MDR-BUG-FLU-00054 — Native video readers can dispatch an unbounded message batch before later video

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rhydra/client-latency
- **Raised:** 2026-08-23T12:09:37Z
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-23T20:15:50Z, deltic:auto role=fix run=fix-20260823T200848Z-6f2d92db branch=task/bug-MDR-BUG-FLU-00054-run-fix-20260823T200848Z-6f2d92db code=b67dff4 gate=manual) -> Closed (2026-09-13T10:41:56Z, 0x4D44/Codex verify run=verify-20260913T102748Z-df5446d7)

## Observation

The production native reader and spike viewer dispatch every complete message from one 64 KiB read before reading or yielding again, and prioritize rect messages. A crafted or pathological batch can run roughly 1600 tiny rect callbacks before a following video callback, delaying decode and paint. Add a bounded dispatch budget or prove a tighter producer-side invariant, while retaining rect priority.

## Fix

The native reader and spike viewer now collect at most 16 completed messages before dispatching. Rect priority remains within each bounded group, while one socket read cannot let a large callback batch delay later video indefinitely.

## Notes

## Verification

The verification build contains fix commit b67dff40e42fa8846fa6646d0acac23a6273d4ec. The viewer net tests passed 13/13, including rect_priority_cannot_invert_an_unbounded_one_read_batch; the native session tests passed 64/64.

For a native behavioral oracle, a disposable test fed 100 cursor messages followed by one video message and stopped on the first cursor callback. The fixed reader passed with exactly 16 callbacks and no video decode. Changing the native fill condition to an unbounded usize::MAX bound made the same test fail with 100 callbacks instead of 16. The temporary test was removed and the source was restored with an empty diff.

As the viewer mutant, its dispatch fill condition was changed to an unbounded usize::MAX bound. The test net::tests::rect_priority_cannot_invert_an_unbounded_one_read_batch failed with exit 101: rect priority delayed one video behind 101 callbacks. Restoring the source made the test pass 1/1. An independent verifier confirmed both caps, reproduced the viewer failure with a fixed oracle, and confirmed matching restored source hashes.

The six repository gates passed on this tree: cargo build --locked; cargo test --locked; cargo fmt --all -- --check; cargo clippy --all-targets --locked -- -D warnings; ./scripts/test-vendored.sh; and ./scripts/check-windows.sh --locked. The Windows gate exited 0 with the existing unused width/height warnings.

No live viewer, Windows, GPU, network, benchmark, or production timing evidence was collected or claimed.
