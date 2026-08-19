# MDR-BUG-FLUX-00013 — Session-lost dialog is a fixed 300 px, so a long failure string pushes Reconnect and Close off the window

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** ui
- **Raised:** 2026-08-19T14:07:49Z
- **Discovery source:** Human
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
- **State history:** Open (2026-08-19T14:07:49Z, raised via `deltic bugs new` model=claude-opus-5@high); Fixed (2026-08-19T14:20:00Z, claude on flux, fix commit 64411fbbc8d2b3c147fef0d2f23d48e22fd9fae5 on task branch — regression tests proven red-then-green, dialog confirmed on screen)

## Observation

Reported by Arthur 2026-08-19 with a screenshot: a session dropped with an
`Unknown GFX PDU type` decode failure, and the Session-lost dialog showed the
heading, the explanation and ten wrapped lines of the IronRDP error chain — and
then nothing. No warning-bar footer, no **Reconnect**, no **Close**. The only way
out of the dialog is the window's close box or Escape/Enter, and the button that
would have resumed the session is not reachable at all.

Mechanism: `AuxWindow::open` builds every dialog `with_resizable(false)`, and
`end_dialog` opened the Session-lost variant at a hard-coded `[480.0, 300.0]`.
The dialog's own content is bounded except for one part — `EndOutcome::Lost`'s
reason string, which is whatever failed. IronRDP renders a failure as a nested
error chain carrying rustc and cargo-registry source paths, so a routine decode
error is ~450 characters and wraps to ten lines at 11 pt mono. Content is stacked
top-down with the footer last, so every line past the window's height pushes the
footer further below the bottom edge, where a non-resizable window simply never
shows it.

The existing fit tests could not catch it: each asserted a fixed size against a
short, hand-written reason ("connection reset by peer"), which is exactly the case
that fits.

## Fix

`end_dialog::window_size` replaces the two size constants. It lays the dialog out
in one headless `egui::Context::run_ui` pass at the fixed width and opens the
window at the height the content actually used — floored at the mock's designed
height (300 warn / 280 ended, so a one-line ending still looks like a dialog) and
capped at `MAX_HEIGHT` 640 so no dialog runs off a small display. `draw` now
returns `Drawn { choice, height }`, the height being the outer frame's laid-out
rect.

Measuring alone is not enough — nothing bounds the reason string, and clamping the
window without bounding the text just recreates the bug at 640 px. So the reason
is drawn inside a `ScrollArea` capped at `REASON_MAX_HEIGHT` (260). Every other
part of the dialog is bounded, so that cap is what guarantees the footer stays
on-window for any string at all.

Validation: two regression tests, both proven red first.
`a_ten_line_failure_grows_the_window_instead_of_losing_the_buttons` carries
Arthur's verbatim error text and fails against the old fixed height ("the dialog
did not grow: [480.0, 300.0]"); `a_runaway_failure_string_stops_growing_the_window`
fails with the scroll cap lifted ("no Reconnect button in a [480.0, 640.0]
window"). The shared fit helper now intersects each galley with its own clip rect,
so text a scroll area has scrolled out of view is not miscounted as overflow.
Confirmed on screen with a throwaway example: the reported failure opens a
480x344 window with both buttons visible. `cargo test --lib ui::`, `cargo fmt`,
`cargo clippy --all-targets -D warnings` all green.

## Notes

Same class as the 440 px About dialog that already overflowed unseen (see
`lessons_learnt.md`): a non-resizable egui window plus text the client does not
author. The other unexpected-ending variant, `ServerEnded`, benefits too — its
detail line comes from MS-RDPBCGR error-info classification and is bounded, but it
is now sized rather than assumed.
