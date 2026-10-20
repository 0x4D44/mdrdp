# MDR-BUG-FLUX-00009 — Exit diagnostic summary and --screenshot are skipped when the session is quit via Cmd+Q / Quit menu

- **State:** Closed
- **Priority:** Should
- **Severity:** Medium
- **Area:** diagnostics
- **Raised:** 2026-08-19T07:16:05Z
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
- **State history:** Open (2026-08-19T07:16:05Z, raised via `deltic bugs new` model=claude-opus-5@high) -> Fixed (2026-08-19T15:15:13Z, deltic:auto role=fix run=fix-20260819T151441Z-p23386-n706098000-c1 branch=task/bug-MDR-BUG-FLUX-00009-run-fix-20260819T151441Z-p23386-n706098000-c1 code=b26b8e7 gate=manual) -> Closed (2026-09-13T06:47:48Z, 0x4D44/Codex verify run=verify-20260913T063113Z-f55cb609)

## Observation

Everything after `window.run()` in `main.rs` is skipped when the user quits the session
the normal way on macOS — Cmd+Q, or the Quit item in the application menu. winit's
`run()` does not return on that path (the process exits inside it; this is the same
mechanism `lessons_learnt.md` already records for clean-up needing to live in
`ApplicationHandler::exiting`). The exit hook's `session ended: …` line still prints
because it runs at `LoopExiting`, so the log looks complete while the diagnostics are
silently gone.

Lost on that path:

- the graphics summary — frames, **decode errors, undecoded regions**, surface errors,
  surfaces created/deleted, ResetGraphics dims, unhandled PDUs, codec mix (`main.rs`,
  the `eprintln!` after `window.run()`);
- **decode failures by reason** and surface failures by reason (`main.rs:1236`) — the
  only place the reason strings are ever emitted;
- the bitmap cache and audio summaries;
- the `--screenshot <file>` capture, so a documented flag silently writes no file.

Measured 2026-08-19, two contrasting sessions on the same build family:

- kiln pid 5837, closed via the Quit menu item (clicked on that exact pid via System
  Events). Log gained exactly one line: `session ended: Graceful`. No summary.
- crucible pid 45006, ended by the *server* ("Another user connected…"), so `run()`
  returned normally. Log carries the full summary: `frames 52534  decode errors 0 …`,
  `surfaces +2 -1  reset Some((2560, 1440))`, codec mix, cache and audio lines.

Impact is not cosmetic. This directly blocked the root-cause diagnosis of
MDR-BUG-FLUX-00008: a session had accumulated 148 decode errors, the reason tally was the
one datum that would have named the failure, and quitting the session to read it is
precisely what destroyed it. Every user-initiated close — the overwhelmingly common case —
discards the diagnostics the client collects.

Expected: the same summary on every exit route, including Cmd+Q and the Quit menu.

## Fix

Extracted the block into `report_session_epilogue()` in `main.rs` and call it from the
window's exit hook, which runs at `LoopExiting` on every close. The call site after
`SessionWindow::run()` remains behind an `AtomicBool` guard, so the case where the hook
did not run is still covered and neither route can produce a second report. The
`--screenshot` capture moves with it, and now happens at `LoopExiting` while the surface
store is still fully populated. The end dialog is unaffected — it genuinely needs the
event loop `run()` hands back, and keeps its own cache snapshot.

**Verified by a controlled red/green on the same host and the same exit route**, quench
2026-08-19, both binaries driven identically (`--size 1280x720`, quit by clicking the
`Quit mdrdp` menu item on that exact pid via System Events):

- **Red** — installed 0.1.69, pre-fix. Log ends at `session ended: Graceful`. Nothing
  after it.
- **Green** — this branch's release build. Same line, then the full summary:
  `frames 42  decode errors 0  undecoded regions 0  surface errors 0`,
  `surfaces +2 -1  reset Some((1280, 720))  unhandled pdus 0`,
  `codecs {"Avc444v2": 39}`, the bitmap cache line and the audio line.

Reproduced green twice more on the same build, including a session driven through six
suppress/resume cycles (`frames 75`, summary intact). Both sessions ended `Graceful`, so
the Shutdown Request still goes out on this path — the fix does not disturb the
disconnect the hook already owned.

Not unit-tested: what broke was *where* the call lived in the winit lifecycle, which no
in-process test can observe. The red/green above is the evidence.

## Notes

## Verification

Independent verification confirmed fix commit `b26b8e7718934ae33170ba6416157ab8a5570d63` and the root lifecycle seam: `report_session_epilogue()` is called from the `on_exit` closure, while the post-`window.run()` call remains only as a guarded fallback. `MDRDP_GUI_TESTS=1 CARGO_TARGET_DIR="$TMPDIR/.../target" deltic timeout 120 cargo test --locked --test macos_menu_lifetime -- --nocapture </dev/null` passed and exercised the macOS `LoopExiting` hook; `cargo test --locked --bin mdrdp -- --nocapture </dev/null` passed all 17 binary tests. As a red root mutant, the hook's `report_session_epilogue(...)` call was removed while leaving the fallback; the source placement oracle then failed its assertion that the `on_exit` closure contains the call. The mutant still compiled with `cargo check --locked --bin mdrdp`.

After restoration, the placement oracle passed, the GUI hook test passed, and the source diff was empty. The full repository gates also passed: `cargo build --locked`, `cargo test --locked` (908 library, 17 binary, 8 integration tests), `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/test-vendored.sh` (372 vendored tests), and `./scripts/check-windows.sh --locked`. A fresh release run against `quench.lan.example` with `--rdp` could not reach the session window and produced no screenshot or diagnostic summary, so no new live Cmd+Q observation is claimed; the original controlled Quench red/green evidence above remains the end-to-end session evidence.
