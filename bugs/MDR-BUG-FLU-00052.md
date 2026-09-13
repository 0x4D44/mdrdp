# MDR-BUG-FLU-00052 — Spike viewer decode-side stats flushes can delay video processing

- **State:** Closed
- **Priority:** Could
- **Severity:** Medium
- **Area:** rhydra/measurement-viewer
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
- **State history:** Open (2026-08-23T12:09:37Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T09:16:17Z, deltic:auto role=fix run=fix-20260824T084803Z-0a28dbc6 branch=task/bug-MDR-BUG-FLU-00052-run-fix-20260824T084803Z-0a28dbc6 code=67e0326 gate=manual) -> Closed (2026-09-13T10:25:11Z, 0x4D44/Codex verify run=verify-20260913T101326Z-2e454244)

## Observation

The latency-spike viewer records several decode-side error, suppression, skip, and displacement rows by writing and flushing its stats file on the same thread that pumps and decodes video. A slow stats destination delays later decode work and distorts the harness. Move or batch those writes so instrumentation does not materially change the path it measures.

## Fix

The viewer now sends decode-side stats through a bounded non-blocking queue to a dedicated flushing writer thread. Queue overload is counted explicitly, and shutdown flushes have a deadline so stats I/O cannot stall video processing.

## Notes

## Verification

The Fixed history records `67e0326018ffdf375fcdb925a4728f98996aef78`, which is a version-only commit. Its parent `86772eddd5400e4b90be4388820610ac450ac87f` is the actual implementation commit: it moves stats writes to a dedicated writer thread, sends records through a bounded non-blocking queue, records overflow, and bounds final flushes. Both commits are ancestors of the verification build.

The viewer `stats::tests` suite passed 20/20 on the verification build. It includes the original blocked-writer regression, queue overload accounting, bounded flush and drop behavior, write-failure reporting, final-tail flushing, and record-size limits. The decode-side recording path now calls the queueing `StatsLog::enqueue`; the recording thread does not wait for the stats writer.

As a red root mutant, the `Full(_)` branch in `tools/latency-spike/viewer/src/stats.rs` was changed to omit `self.dropped.fetch_add(1, Ordering::Relaxed)`. `stats::tests::queue_overload_is_bounded_and_recorded_after_the_writer_recovers` then failed with exit 101 at its count assertion: `left: 0`, `right: 10`. The source was restored with an empty diff, and all 20 stats tests passed again. The independent verifier also mutated the emitted drop count to zero; the same overload test failed at `overload must be visible in the stats stream`, then passed after restoration.

The six repository gates passed on this tree: `cargo build --locked`; `cargo test --locked`; `cargo fmt --all -- --check`; `cargo clippy --all-targets --locked -- -D warnings`; `./scripts/test-vendored.sh`; and `./scripts/check-windows.sh --locked`. The Windows gate exited 0 with only the existing icon and unused `width`/`height` warnings.

No live viewer, Windows, GPU, network, or production timing evidence was collected or claimed.
