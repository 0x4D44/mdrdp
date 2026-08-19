# MDR-BUG-FLUX-00009 — Exit diagnostic summary and --screenshot are skipped when the session is quit via Cmd+Q / Quit menu

- **State:** Open
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

<unfixed — raised only>

## Notes
