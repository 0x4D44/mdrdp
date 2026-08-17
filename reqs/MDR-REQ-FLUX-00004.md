# MDR-REQ-FLUX-00004 — Mac-faithful keyboard mode: Unicode input for printables behind a Settings toggle

- **State:** Draft
- **Priority:** Should
- **Area:** input
- **Raised:** 2026-08-17T21:52:27Z
- **Discovery source:** Human
- **Implemented-by:** —
- **Satisfied-by:** —
- **Violated-by:** —
- **Depends-on:** —
- **Design:** —
- **Flow:** heavy
- **Claimed-by:** —
- **State history:** Draft (2026-08-17T21:52:27Z, raised via `deltic reqs new`)

## Statement

With Settings ▸ Keyboard ▸ "Mac keyboard mode" enabled, every printable keypress must
type the character its Mac keycap produces (the layout-resolved character macOS reports,
dead-key composition included), regardless of the server's keyboard layout: the client
sends `FASTPATH_INPUT_EVENT_UNICODE` for printable characters, while navigation keys,
function keys, Enter/Tab/Escape/Backspace, and any key pressed with Ctrl/Cmd/Alt held
continue to go as scancodes so server-side shortcuts still see the chord. With the
toggle off (the default), behaviour is unchanged: positional scancodes, matching
Microsoft's Windows App. Oracle: against quench (UK Windows layout) with the Mac on
Apple British, the keys `@ " \ # § ` ~ | £` each type their keycap character in remote
Notepad with the mode on, and Cmd/Ctrl+C-style chords still copy; with the mode off the
existing positional behaviour (validated 2026-08-17) is preserved.

## Notes

Decided with Arthur 2026-08-17 while fixing the ISO 102nd-key bug (winit folds both ISO
corner keys into `Backquote`; see `wrk_journals/2026.08.17 - JRN - ISO 102nd key
recovered from winit Backquote collapse.md`). Positional parity stays the default; this
mode serves Mac muscle memory, and also resolves the documented trade-off where a
PC-emulating Mac layout ("British – PC") gets its top-left backtick delivered as `\`.
IronRDP exposes the Unicode event as `FastPathInputEvent::UnicodeKeyboardEvent`.
Design points: key-up pairing for Unicode events, auto-repeat, and Shift-only presses
must take the Unicode path (that is the point of the mode).
