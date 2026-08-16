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
//! ```
//!
//! Key names for `keys`: letters, digits, `enter`, `esc`, `tab`, `space`, `backspace`,
//! `delete`, `win`, `ctrl`, `alt`, `shift`, `f1`–`f12`, arrows (`up`, `down`, `left`,
//! `right`). `type` covers printable ASCII on the US layout.
//!
//! **This is an input source, not input logging.** Script contents are the operator's
//! own commands; nothing here reads or records what the session sends otherwise.

use std::sync::mpsc::Sender;
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
                "click" => {
                    let mut parts = rest.split_whitespace();
                    let x = parts
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| format!("line {}: click wants x y", idx + 1))?;
                    let y = parts
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| format!("line {}: click wants x y", idx + 1))?;
                    Step::Click { x, y }
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
    pub fn spawn(self, input: Sender<InputEvent>) {
        std::thread::Builder::new()
            .name("mdrdp-input-script".to_owned())
            .spawn(move || self.run(&input))
            .map(|_| ())
            .unwrap_or_else(|e| eprintln!("input script: could not start: {e}"));
    }

    fn run(&self, input: &Sender<InputEvent>) {
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
                Step::Click { x, y } => {
                    let down = InputEvent::MouseButton {
                        button: MouseButton::Left,
                        down: true,
                        x: *x,
                        y: *y,
                    };
                    let up = InputEvent::MouseButton {
                        button: MouseButton::Left,
                        down: false,
                        x: *x,
                        y: *y,
                    };
                    input.send(InputEvent::MouseMove { x: *x, y: *y }).is_ok() && {
                        std::thread::sleep(KEY_GAP);
                        input.send(down).is_ok() && {
                            std::thread::sleep(KEY_HOLD);
                            input.send(up).is_ok()
                        }
                    }
                }
            };
            if !ok {
                return; // session gone; nothing left to type into
            }
        }
    }
}

/// Press every code in order, hold, release in reverse. True while the channel lives.
fn send_chord(input: &Sender<InputEvent>, codes: &[Scancode]) -> bool {
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
        script.run(&tx);
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
        script.run(&tx);
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
    fn a_click_moves_then_presses_then_releases() {
        let script = Script::parse("click 100 200").expect("parses");
        let (tx, rx) = mpsc::channel();
        script.run(&tx);
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
