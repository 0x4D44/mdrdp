//! Coloured command-line output: the version banner and the `--help` screen.
//!
//! Both come in a plain and a coloured form, rendered from the same data, so what a
//! script parses and what a person reads can never drift apart. Colour is only ever
//! emitted when the caller asks for it; [`stdout_wants_color`] holds the one policy
//! decision (a real terminal, and `NO_COLOR` unset).

use std::io::IsTerminal;

pub const NAME: &str = env!("CARGO_PKG_NAME");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The gradient the banner cycles through, one colour per visible character.
const RAINBOW: &[(u8, u8, u8)] = &[
    (255, 95, 95),   // red
    (255, 165, 0),   // orange
    (240, 220, 0),   // yellow
    (95, 215, 95),   // green
    (0, 195, 255),   // cyan
    (135, 135, 255), // blue
    (215, 135, 255), // violet
];

/// Whether coloured output belongs on stdout right now.
///
/// `NO_COLOR` (the informal cross-tool convention) always wins; otherwise colour goes
/// only to a real terminal, never into a pipe someone will parse.
pub fn stdout_wants_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// Like [`stdout_wants_color`], for the stderr status stream.
pub fn stderr_wants_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

fn sgr(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

/// `mdrdp v0.1.29`, each visible character stepping through the rainbow when coloured.
pub fn version_banner(color: bool) -> String {
    let plain = format!("{NAME} v{VERSION}");
    if !color {
        return plain;
    }
    let mut out = String::new();
    let mut hue = 0usize;
    for ch in plain.chars() {
        if ch.is_whitespace() {
            out.push(ch);
            continue;
        }
        let (r, g, b) = RAINBOW[hue % RAINBOW.len()];
        out.push_str(&format!("\x1b[1;38;2;{r};{g};{b}m{ch}"));
        hue += 1;
    }
    out.push_str("\x1b[0m");
    out
}

const USAGE: &[(&str, &str)] = &[
    ("mdrdp", "pick from the favourites launcher"),
    ("mdrdp <favourite>", "connect to a saved favourite by name"),
    (
        "mdrdp <host> --user <account>",
        "connect to a host directly",
    ),
    ("mdrdp --sessions", "list the live sessions and their stats"),
];

const OPTIONS: &[(&str, &str)] = &[
    (
        "--user, -u <account>",
        "keychain account holding the password",
    ),
    ("--port, -p <n>", "default 3389"),
    ("--domain, -d <d>", "Windows domain"),
    ("--size, -s WxH", "session resolution, e.g. 1920x1080"),
    (
        "--fullscreen, -f",
        "open fullscreen (and renegotiate to the native resolution)",
    ),
    (
        "--foreground, -F",
        "stay attached to the terminal (a GUI run normally detaches\nand logs under the config directory)",
    ),
    ("--list, -l", "print saved favourites and exit"),
    (
        "--sessions, -S",
        "list this machine's live sessions, one line of stats each",
    ),
    (
        "--duration, -t <secs>",
        "disconnect cleanly after N seconds (for scripted runs)",
    ),
    (
        "--password-stdin",
        "read the password from stdin instead of the keychain",
    ),
    (
        "--ask-password",
        "ask the driving launcher for a fresh password instead of\nusing the saved one (needs --stage-json)",
    ),
    (
        "--screenshot <file>",
        "write the final frame to a BMP (session pixels on disk)",
    ),
    (
        "--metrics-json <file>",
        "write a redacted session metrics report as JSON",
    ),
    (
        "--input-script <file>",
        "inject scripted keystrokes into the session (for tests)",
    ),
    (
        "--stage-json",
        "print machine-readable connect progress on stdout",
    ),
    (
        "--capture-failures <dir>",
        "dump undecodable tiles AND the first raw AVC444 frames\n(screen content!) for offline debugging",
    ),
    ("--version, -V", "print the version and exit"),
    ("--help, -h", "show this help"),
];

const FOOTER: &str = "Flags override whatever the chosen favourite specifies. A [defaults] username in\n\
     favourites.toml is used when neither a flag nor a favourite names an account.";

/// The full `--help` screen, version banner first.
///
/// The column layout is identical with and without colour — the escape sequences are
/// added around the already-padded cells — so the plain form is exactly the coloured
/// form with the ANSI codes stripped.
pub fn help_text(color: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(version_banner(color));
    lines.push(String::new());
    lines.push(sgr("usage:", "1;33", color));
    for (invocation, what) in USAGE {
        lines.push(format!(
            "  {} {}",
            sgr(&format!("{invocation:<32}"), "1", color),
            sgr(what, "2", color)
        ));
    }
    lines.push(String::new());
    lines.push(sgr("options:", "1;33", color));
    for (flag, what) in OPTIONS {
        let mut description = what.lines();
        let first = description.next().unwrap_or("");
        lines.push(format!(
            "  {} {}",
            sgr(&format!("{flag:<23}"), "36", color),
            sgr(first, "2", color)
        ));
        for continuation in description {
            lines.push(format!("  {:<23} {}", "", sgr(continuation, "2", color)));
        }
    }
    lines.push(String::new());
    lines.push(FOOTER.to_owned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for e in chars.by_ref() {
                    if e == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn the_plain_banner_is_name_and_version_and_nothing_else() {
        assert_eq!(version_banner(false), format!("mdrdp v{VERSION}"));
        assert!(!version_banner(false).contains('\x1b'));
    }

    #[test]
    fn the_coloured_banner_is_the_plain_banner_underneath() {
        assert_eq!(strip_ansi(&version_banner(true)), version_banner(false));
    }

    #[test]
    fn the_banner_is_a_rainbow_not_one_colour() {
        let banner = version_banner(true);
        let distinct: std::collections::HashSet<&str> = banner
            .split('\x1b')
            .filter(|s| s.contains("38;2;"))
            .filter_map(|s| s.split('m').next())
            .collect();
        assert!(
            distinct.len() >= 5,
            "expected a cycling palette, got {distinct:?}"
        );
    }

    #[test]
    fn coloured_help_is_plain_help_underneath() {
        assert_eq!(strip_ansi(&help_text(true)), help_text(false));
    }

    #[test]
    fn help_leads_with_the_version_banner() {
        assert!(help_text(false).starts_with(&version_banner(false)));
        assert!(help_text(true).starts_with(&version_banner(true)));
    }

    #[test]
    fn plain_help_documents_every_flag_main_accepts() {
        let text = help_text(false);
        for flag in [
            "--user",
            "--port",
            "--domain",
            "--size",
            "--fullscreen",
            "--foreground",
            "--list",
            "--sessions",
            "--duration",
            "--password-stdin",
            "--ask-password",
            "--screenshot",
            "--metrics-json",
            "--input-script",
            "--stage-json",
            "--capture-failures",
            "--version",
            "--help",
        ] {
            assert!(text.contains(flag), "help is missing {flag}");
        }
        assert!(!text.contains('\x1b'), "plain help must carry no colour");
    }

    #[test]
    fn help_documents_every_short_code_next_to_its_long_flag() {
        let text = help_text(false);
        for pair in [
            "--user, -u",
            "--port, -p",
            "--domain, -d",
            "--size, -s",
            "--fullscreen, -f",
            "--foreground, -F",
            "--list, -l",
            "--sessions, -S",
            "--duration, -t",
            "--version, -V",
            "--help, -h",
        ] {
            assert!(text.contains(pair), "help is missing {pair}");
        }
    }

    #[test]
    fn no_short_code_is_claimed_twice() {
        let text = help_text(false);
        let mut seen = std::collections::HashSet::new();
        for token in text.split_whitespace() {
            let token = token.trim_end_matches(',');
            if token.len() == 2
                && token.starts_with('-')
                && !token.starts_with("--")
                && !seen.insert(token.to_owned())
            {
                panic!("short code {token} appears against two flags");
            }
        }
        assert!(seen.len() >= 11, "expected the full short-code set");
    }

    #[test]
    fn help_documents_the_redacted_metrics_export() {
        let text = help_text(false);
        assert!(text.contains("--metrics-json <file>"));
        assert!(text.contains("redacted"));
    }
}
