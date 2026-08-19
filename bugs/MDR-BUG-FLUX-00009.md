# MDR-BUG-FLUX-00009 — Exit diagnostic summary and --screenshot are skipped when the session is quit via Cmd+Q / Quit menu

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** diagnostics
- **Raised:** 2026-08-19T07:16:05Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260819T151441Z-p23386-n706098000-c1
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLUX-00009-run-fix-20260819T151441Z-p23386-n706098000-c1
- **Owner base:** daf875b9d1b5589bbb43538c4139237e05353522
- **Owner fingerprint:** -
- **Owner since:** 2026-08-19T15:14:41Z
- **Owner until:** 2026-08-19T17:14:41Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-19T07:16:05Z, raised via `deltic bugs new` model=claude-opus-5@high)

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
