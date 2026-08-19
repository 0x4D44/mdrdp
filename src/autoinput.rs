//! Scripted input injection for unattended test runs.
//!
//! `--input-script <file>` feeds keystrokes straight into the session's input channel,
//! bypassing the window system entirely. macOS focus, Spaces, and display sleep have
//! all silently eaten scripted `osascript` keystrokes during live debugging — a run
//! looks healthy while the remote never receives a thing. Injecting [`InputEvent`]s
//! directly is the only path a script can actually rely on.
//!
//! The script format is one command per line; `#` starts a comment:
//!
//! ```text
//! sleep 12          # seconds, fractions allowed
//! keys win+r        # a chord: modifiers pressed in order, released in reverse
//! type msedge https://example.com
//! keys enter
//! click 960 540     # left-click at a session pixel
//! rclick 960 540    # right-click
//! dblclick 960 540  # double-click (left, inside the double-click threshold)
//! scroll 960 540 -3 # vertical wheel at a point; positive up, negative down
//! drag 500 20 900 400 800   # press at 500,20, move to 900,400 over 800 ms, release
//! ```
//!
//! Key names for `keys`: letters, digits, `enter`, `esc`, `tab`, `space`, `backspace`,
//! `delete`, `win`, `ctrl`, `alt`, `shift`, `f1`–`f12`, arrows (`up`, `down`, `left`,
//! `right`). `type` covers printable ASCII on the US layout.
//!
//! **This is an input source, not input logging.** Script contents are the operator's
//! own commands; nothing here reads or records what the session sends otherwise.

use crate::wake::WakingSender;
use std::time::Duration;

use crate::input::{InputEvent, MouseButton, Scancode};

/// One parsed script step.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Sleep(Duration),
    /// Scancodes pressed in order and released in reverse.
    Chord(Vec<Scancode>),
    /// Each entry is one key tap, with shift held where needed.
    Type(Vec<(Scancode, bool)>),
    Click {
        x: u16,
        y: u16,
        button: MouseButton,
    },
    /// Two left clicks inside the double-click threshold.
    DoubleClick {
        x: u16,
        y: u16,
    },
    /// A vertical wheel turn at a point, counted in notches (detents), positive
    /// scrolling up (away). One notch becomes one `InputEvent::Scroll` of
    /// [`crate::input::WHEEL_UNITS_PER_NOTCH`] units — see [`send_scroll`].
    Scroll {
        x: u16,
        y: u16,
        notches: i16,
    },
    /// Press at `from`, move to `to` in interpolated steps, release. A window drag.
    Drag {
        from: (u16, u16),
        to: (u16, u16),
        duration: Duration,
    },
}

/// A parsed input script, ready to run against a session.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    steps: Vec<Step>,
}

/// How long a key stays down, and the gap between taps. Fast enough for a script,
/// slow enough that remote key repeat and ordering behave like a human typist.
const KEY_HOLD: Duration = Duration::from_millis(35);
const KEY_GAP: Duration = Duration::from_millis(25);

impl Script {
    /// Parse a script. Errors carry the 1-based line number.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut steps = Vec::new();
        for (idx, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (cmd, rest) = match line.split_once(char::is_whitespace) {
                Some((c, r)) => (c, r.trim()),
                None => (line, ""),
            };
            let step = match cmd {
                "sleep" => {
                    let secs: f64 = rest.parse().map_err(|_| {
                        format!("line {}: sleep wants seconds, got {rest:?}", idx + 1)
                    })?;
                    if !(0.0..=3600.0).contains(&secs) {
                        return Err(format!("line {}: sleep out of range", idx + 1));
                    }
                    Step::Sleep(Duration::from_secs_f64(secs))
                }
                "keys" => {
                    let mut codes = Vec::new();
                    for name in rest.split('+') {
                        codes.push(named_key(name.trim()).ok_or_else(|| {
                            format!("line {}: unknown key {:?}", idx + 1, name.trim())
                        })?);
                    }
                    if codes.is_empty() {
                        return Err(format!("line {}: keys wants at least one key", idx + 1));
                    }
                    Step::Chord(codes)
                }
                "type" => {
                    let mut taps = Vec::new();
                    for ch in rest.chars() {
                        taps.push(char_key(ch).ok_or_else(|| {
                            format!("line {}: cannot type {ch:?} on the US layout", idx + 1)
                        })?);
                    }
                    Step::Type(taps)
                }
                "click" | "rclick" | "dblclick" | "scroll" => {
                    let mut parts = rest.split_whitespace();
                    let mut coord = || -> Option<u16> { parts.next()?.parse().ok() };
                    let (Some(x), Some(y)) = (coord(), coord()) else {
                        return Err(format!("line {}: {cmd} wants x y", idx + 1));
                    };
                    match cmd {
                        "click" => Step::Click {
                            x,
                            y,
                            button: MouseButton::Left,
                        },
                        "rclick" => Step::Click {
                            x,
                            y,
                            button: MouseButton::Right,
                        },
                        "dblclick" => Step::DoubleClick { x, y },
                        _ => {
                            let notches: i16 =
                                parts.next().and_then(|v| v.parse().ok()).ok_or_else(|| {
                                    format!("line {}: scroll wants x y notches", idx + 1)
                                })?;
                            if notches == 0 {
                                return Err(format!("line {}: scroll of zero notches", idx + 1));
                            }
                            Step::Scroll { x, y, notches }
                        }
                    }
                }
                "drag" => {
                    let mut parts = rest.split_whitespace();
                    let mut coord = || -> Option<u16> { parts.next()?.parse().ok() };
                    let (x1, y1, x2, y2) = (coord(), coord(), coord(), coord());
                    let (Some(x1), Some(y1), Some(x2), Some(y2)) = (x1, y1, x2, y2) else {
                        return Err(format!("line {}: drag wants x1 y1 x2 y2 [ms]", idx + 1));
                    };
                    let ms: u64 = match parts.next() {
                        Some(v) => v.parse().map_err(|_| {
                            format!("line {}: drag duration wants milliseconds", idx + 1)
                        })?,
                        None => 800,
                    };
                    if !(1..=60_000).contains(&ms) {
                        return Err(format!("line {}: drag duration out of range", idx + 1));
                    }
                    Step::Drag {
                        from: (x1, y1),
                        to: (x2, y2),
                        duration: Duration::from_millis(ms),
                    }
                }
                other => return Err(format!("line {}: unknown command {other:?}", idx + 1)),
            };
            steps.push(step);
        }
        Ok(Script { steps })
    }

    /// Run the script on its own thread, sending into the session's input channel.
    ///
    /// A closed channel (the session ended) just stops the script — an unattended run
    /// that outlives its session is normal, not an error worth surfacing.
    pub fn spawn(self, input: WakingSender<InputEvent>) {
        std::thread::Builder::new()
            .name("mdrdp-input-script".to_owned())
            .spawn(move || self.run(&input))
            .map(|_| ())
            .unwrap_or_else(|e| eprintln!("input script: could not start: {e}"));
    }

    fn run(&self, input: &WakingSender<InputEvent>) {
        for step in &self.steps {
            let ok = match step {
                Step::Sleep(d) => {
                    std::thread::sleep(*d);
                    true
                }
                Step::Chord(codes) => send_chord(input, codes),
                Step::Type(taps) => taps.iter().all(|(code, shift)| {
                    let sent = if *shift {
                        send_chord(input, &[named_key("shift").expect("shift exists"), *code])
                    } else {
                        send_chord(input, &[*code])
                    };
                    std::thread::sleep(KEY_GAP);
                    sent
                }),
                Step::Click { x, y, button } => send_click(input, *button, *x, *y),
                Step::DoubleClick { x, y } => {
                    send_click(input, MouseButton::Left, *x, *y) && {
                        // Well inside Windows' default 500 ms double-click window.
                        std::thread::sleep(Duration::from_millis(80));
                        send_click(input, MouseButton::Left, *x, *y)
                    }
                }
                Step::Scroll { x, y, notches } => send_scroll(input, *x, *y, *notches),
                Step::Drag { from, to, duration } => send_drag(input, *from, *to, *duration),
            };
            if !ok {
                return; // session gone; nothing left to type into
            }
        }
    }
}

/// Move to the point, then turn the wheel one notch at a time.
///
/// A script counts notches, because that is what a reader means by "scroll down
/// three". [`InputEvent::Scroll`] does not: its `units` are wheel units, of which
/// one notch is [`crate::input::WHEEL_UNITS_PER_NOTCH`] — real scroll input emits
/// one event of ±120 per notch, and the native wire refuses anything that is not
/// a multiple of 120 outright. Sending the notch count raw got the input channel
/// closed by the host mid-run ("wheel delta -3 is not a nonzero multiple of 120").
fn send_scroll(input: &WakingSender<InputEvent>, x: u16, y: u16, notches: i16) -> bool {
    if input.send(InputEvent::MouseMove { x, y }).is_err() {
        return false;
    }
    let step = i16::from(crate::input::WHEEL_UNITS_PER_NOTCH as i16) * notches.signum();
    for _ in 0..notches.unsigned_abs() {
        std::thread::sleep(KEY_GAP);
        let sent = input
            .send(InputEvent::Scroll {
                axis: crate::input::ScrollAxis::Vertical,
                units: step,
                x,
                y,
            })
            .is_ok();
        if !sent {
            return false;
        }
    }
    true
}

/// Move to the point, press, hold briefly, release. True while the channel lives.
fn send_click(input: &WakingSender<InputEvent>, button: MouseButton, x: u16, y: u16) -> bool {
    input.send(InputEvent::MouseMove { x, y }).is_ok() && {
        std::thread::sleep(KEY_GAP);
        input
            .send(InputEvent::MouseButton {
                button,
                down: true,
                x,
                y,
            })
            .is_ok()
            && {
                std::thread::sleep(KEY_HOLD);
                input
                    .send(InputEvent::MouseButton {
                        button,
                        down: false,
                        x,
                        y,
                    })
                    .is_ok()
            }
    }
}

/// Press at `from`, walk to `to` in ~16 ms interpolated moves, release at `to`.
/// True while the channel lives. The pacing matters: Windows treats an instant
/// press-jump-release as a click at the destination, not a drag.
fn send_drag(
    input: &WakingSender<InputEvent>,
    from: (u16, u16),
    to: (u16, u16),
    duration: Duration,
) -> bool {
    const TICK: Duration = Duration::from_millis(16);
    let steps = (duration.as_millis() / TICK.as_millis()).clamp(2, 400) as u32;

    if input
        .send(InputEvent::MouseMove {
            x: from.0,
            y: from.1,
        })
        .is_err()
    {
        return false;
    }
    std::thread::sleep(KEY_GAP);
    if input
        .send(InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
            x: from.0,
            y: from.1,
        })
        .is_err()
    {
        return false;
    }
    // A short hold before moving, so the remote registers press-then-drag.
    std::thread::sleep(KEY_HOLD);

    let lerp = |a: u16, b: u16, i: u32| -> u16 {
        let a = f64::from(a);
        let b = f64::from(b);
        let t = f64::from(i) / f64::from(steps);
        (a + (b - a) * t).round() as u16
    };
    for i in 1..=steps {
        let (x, y) = (lerp(from.0, to.0, i), lerp(from.1, to.1, i));
        if input.send(InputEvent::MouseMove { x, y }).is_err() {
            return false;
        }
        std::thread::sleep(TICK);
    }

    std::thread::sleep(KEY_HOLD);
    input
        .send(InputEvent::MouseButton {
            button: MouseButton::Left,
            down: false,
            x: to.0,
            y: to.1,
        })
        .is_ok()
}

/// Press every code in order, hold, release in reverse. True while the channel lives.
fn send_chord(input: &WakingSender<InputEvent>, codes: &[Scancode]) -> bool {
    for code in codes {
        if input
            .send(InputEvent::Key {
                scancode: *code,
                down: true,
            })
            .is_err()
        {
            return false;
        }
        std::thread::sleep(KEY_GAP);
    }
    std::thread::sleep(KEY_HOLD);
    for code in codes.iter().rev() {
        if input
            .send(InputEvent::Key {
                scancode: *code,
                down: false,
            })
            .is_err()
        {
            return false;
        }
        std::thread::sleep(KEY_GAP);
    }
    true
}

/// Scancode for a named key in a `keys` chord.
fn named_key(name: &str) -> Option<Scancode> {
    let sc = match name {
        "win" => Scancode::extended(0x5B),
        "ctrl" => Scancode::plain(0x1D),
        "alt" => Scancode::plain(0x38),
        "shift" => Scancode::plain(0x2A),
        "enter" => Scancode::plain(0x1C),
        "esc" => Scancode::plain(0x01),
        "tab" => Scancode::plain(0x0F),
        "space" => Scancode::plain(0x39),
        "backspace" => Scancode::plain(0x0E),
        "delete" => Scancode::extended(0x53),
        "up" => Scancode::extended(0x48),
        "down" => Scancode::extended(0x50),
        "left" => Scancode::extended(0x4B),
        "right" => Scancode::extended(0x4D),
        "f1" => Scancode::plain(0x3B),
        "f2" => Scancode::plain(0x3C),
        "f3" => Scancode::plain(0x3D),
        "f4" => Scancode::plain(0x3E),
        "f5" => Scancode::plain(0x3F),
        "f6" => Scancode::plain(0x40),
        "f7" => Scancode::plain(0x41),
        "f8" => Scancode::plain(0x42),
        "f9" => Scancode::plain(0x43),
        "f10" => Scancode::plain(0x44),
        "f11" => Scancode::plain(0x57),
        "f12" => Scancode::plain(0x58),
        single if single.len() == 1 => {
            let ch = single.chars().next()?;
            return char_key(ch).map(|(code, _)| code);
        }
        _ => return None,
    };
    Some(sc)
}

/// (scancode, needs-shift) for a printable ASCII character on the US layout.
fn char_key(ch: char) -> Option<(Scancode, bool)> {
    let plain = |code: u8| Some((Scancode::plain(code), false));
    let shifted = |code: u8| Some((Scancode::plain(code), true));
    match ch {
        'a'..='z' | 'A'..='Z' => {
            let shift = ch.is_ascii_uppercase();
            let code = match ch.to_ascii_lowercase() {
                'a' => 0x1E,
                'b' => 0x30,
                'c' => 0x2E,
                'd' => 0x20,
                'e' => 0x12,
                'f' => 0x21,
                'g' => 0x22,
                'h' => 0x23,
                'i' => 0x17,
                'j' => 0x24,
                'k' => 0x25,
                'l' => 0x26,
                'm' => 0x32,
                'n' => 0x31,
                'o' => 0x18,
                'p' => 0x19,
                'q' => 0x10,
                'r' => 0x13,
                's' => 0x1F,
                't' => 0x14,
                'u' => 0x16,
                'v' => 0x2F,
                'w' => 0x11,
                'x' => 0x2D,
                'y' => 0x15,
                'z' => 0x2C,
                _ => unreachable!(),
            };
            Some((Scancode::plain(code), shift))
        }
        '1' => plain(0x02),
        '2' => plain(0x03),
        '3' => plain(0x04),
        '4' => plain(0x05),
        '5' => plain(0x06),
        '6' => plain(0x07),
        '7' => plain(0x08),
        '8' => plain(0x09),
        '9' => plain(0x0A),
        '0' => plain(0x0B),
        '!' => shifted(0x02),
        '@' => shifted(0x03),
        '#' => shifted(0x04),
        '$' => shifted(0x05),
        '%' => shifted(0x06),
        '^' => shifted(0x07),
        '&' => shifted(0x08),
        '*' => shifted(0x09),
        '(' => shifted(0x0A),
        ')' => shifted(0x0B),
        '-' => plain(0x0C),
        '_' => shifted(0x0C),
        '=' => plain(0x0D),
        '+' => shifted(0x0D),
        '[' => plain(0x1A),
        '{' => shifted(0x1A),
        ']' => plain(0x1B),
        '}' => shifted(0x1B),
        '\\' => plain(0x2B),
        '|' => shifted(0x2B),
        ';' => plain(0x27),
        ':' => shifted(0x27),
        '\'' => plain(0x28),
        '"' => shifted(0x28),
        '`' => plain(0x29),
        '~' => shifted(0x29),
        ',' => plain(0x33),
        '<' => shifted(0x33),
        '.' => plain(0x34),
        '>' => shifted(0x34),
        '/' => plain(0x35),
        '?' => shifted(0x35),
        ' ' => plain(0x39),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_chord_presses_in_order_and_releases_in_reverse() {
        let script = Script::parse("keys win+r").expect("parses");
        let (tx, rx) = mpsc::channel();
        script.run(&WakingSender::silent(tx));
        let events: Vec<InputEvent> = rx.try_iter().collect();
        assert_eq!(
            events,
            vec![
                InputEvent::Key {
                    scancode: Scancode::extended(0x5B),
                    down: true
                },
                InputEvent::Key {
                    scancode: Scancode::plain(0x13),
                    down: true
                },
                InputEvent::Key {
                    scancode: Scancode::plain(0x13),
                    down: false
                },
                InputEvent::Key {
                    scancode: Scancode::extended(0x5B),
                    down: false
                },
            ],
            "Win must be a real key event held around the letter, or the remote sees a bare 'r'"
        );
    }

    #[test]
    fn typing_a_url_produces_shift_only_where_needed() {
        let script = Script::parse("type a:/B").expect("parses");
        let (tx, rx) = mpsc::channel();
        script.run(&WakingSender::silent(tx));
        let events: Vec<InputEvent> = rx.try_iter().collect();
        // a (2 events), shift+: (4), / (2), shift+B (4)
        assert_eq!(events.len(), 12);
        let shift_downs = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    InputEvent::Key {
                        scancode: Scancode {
                            code: 0x2A,
                            extended: false
                        },
                        down: true
                    }
                )
            })
            .count();
        assert_eq!(
            shift_downs, 2,
            "one shift for ':', one for 'B', none for 'a' or '/'"
        );
    }

    #[test]
    fn rclick_dblclick_and_scroll_produce_the_right_events() {
        let script =
            Script::parse("rclick 10 20\ndblclick 30 40\nscroll 50 60 -3").expect("parses");
        let (tx, rx) = mpsc::channel();
        script.run(&WakingSender::silent(tx));
        let events: Vec<InputEvent> = rx.try_iter().collect();

        // rclick: move, right down, right up.
        assert_eq!(events[0], InputEvent::MouseMove { x: 10, y: 20 });
        assert!(matches!(
            events[1],
            InputEvent::MouseButton {
                button: MouseButton::Right,
                down: true,
                x: 10,
                y: 20,
            }
        ));
        assert!(matches!(
            events[2],
            InputEvent::MouseButton {
                button: MouseButton::Right,
                down: false,
                ..
            }
        ));

        // dblclick: two full left clicks (move+down+up, twice) at the same point.
        let left_downs = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    InputEvent::MouseButton {
                        button: MouseButton::Left,
                        down: true,
                        x: 30,
                        y: 40,
                    }
                )
            })
            .count();
        assert_eq!(left_downs, 2, "a double-click is two presses");

        // scroll: move, then ONE EVENT PER NOTCH, each carrying a whole wheel
        // notch (120 units) — not the notch count. The native wire refuses a
        // delta that is not a nonzero multiple of 120 and closes the channel, so
        // a `units: -3` here is a live-session failure, not a cosmetic one.
        let scrolls: Vec<&InputEvent> = events
            .iter()
            .filter(|e| matches!(e, InputEvent::Scroll { .. }))
            .collect();
        assert_eq!(scrolls.len(), 3, "three notches are three wheel events");
        for scroll in scrolls {
            assert_eq!(
                scroll,
                &InputEvent::Scroll {
                    axis: crate::input::ScrollAxis::Vertical,
                    units: -crate::input::WHEEL_UNITS_PER_NOTCH as i16,
                    x: 50,
                    y: 60,
                }
            );
        }
    }

    #[test]
    fn scroll_without_units_or_zero_units_is_a_parse_error() {
        assert!(Script::parse("scroll 10 20").is_err());
        assert!(Script::parse("scroll 10 20 0").is_err());
    }

    #[test]
    fn a_click_moves_then_presses_then_releases() {
        let script = Script::parse("click 100 200").expect("parses");
        let (tx, rx) = mpsc::channel();
        script.run(&WakingSender::silent(tx));
        let events: Vec<InputEvent> = rx.try_iter().collect();
        assert_eq!(events[0], InputEvent::MouseMove { x: 100, y: 200 });
        assert!(matches!(
            events[1],
            InputEvent::MouseButton {
                button: MouseButton::Left,
                down: true,
                ..
            }
        ));
        assert!(matches!(
            events[2],
            InputEvent::MouseButton {
                button: MouseButton::Left,
                down: false,
                ..
            }
        ));
    }

    #[test]
    fn a_drag_presses_at_the_start_and_releases_at_the_end() {
        // 100 ms → few interpolation steps, so the test stays fast.
        let script = Script::parse("drag 100 20 300 220 100").expect("parses");
        let (tx, rx) = mpsc::channel();
        script.run(&WakingSender::silent(tx));
        let events: Vec<InputEvent> = rx.try_iter().collect();

        assert_eq!(events[0], InputEvent::MouseMove { x: 100, y: 20 });
        assert_eq!(
            events[1],
            InputEvent::MouseButton {
                button: MouseButton::Left,
                down: true,
                x: 100,
                y: 20,
            },
            "the press belongs at the START — pressing at the destination is a click, not a drag"
        );
        assert_eq!(
            *events.last().expect("events exist"),
            InputEvent::MouseButton {
                button: MouseButton::Left,
                down: false,
                x: 300,
                y: 220,
            }
        );
        // Between press and release: only moves, ending exactly at the destination.
        let moves: Vec<(u16, u16)> = events[2..events.len() - 1]
            .iter()
            .map(|e| match e {
                InputEvent::MouseMove { x, y } => (*x, *y),
                other => panic!("unexpected event mid-drag: {other:?}"),
            })
            .collect();
        assert!(
            moves.len() >= 2,
            "a drag interpolates, it does not teleport"
        );
        assert_eq!(*moves.last().expect("moves exist"), (300, 220));

        let err = Script::parse("drag 1 2 3").unwrap_err();
        assert!(err.contains("line 1"), "got: {err}");
    }

    #[test]
    fn comments_blanks_and_errors_are_reported_with_line_numbers() {
        let ok = Script::parse("# a comment\n\nsleep 0.5\nkeys enter\n");
        assert_eq!(ok.expect("parses").steps.len(), 2);

        let err = Script::parse("keys nosuchkey").unwrap_err();
        assert!(err.contains("line 1"), "got: {err}");
        let err = Script::parse("sleep fast").unwrap_err();
        assert!(err.contains("line 1"), "got: {err}");
        let err = Script::parse("type \u{263A}").unwrap_err();
        assert!(err.contains("line 1"), "got: {err}");
    }
}
