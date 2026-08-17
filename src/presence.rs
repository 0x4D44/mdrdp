//! Live cross-process session presence — what `mdrdp --sessions` reads.
//!
//! Every session is its own OS process, so "list the active sessions" needs a
//! rendezvous on disk: each session keeps one small JSON file alive under
//! `<config dir>/sessions/<pid>.json`, rewritten every [`WRITE_INTERVAL`], and the
//! lister shows every file fresh enough to belong to a living process. Freshness is
//! the liveness signal — a crashed or killed session simply stops updating and its
//! file goes stale — so no platform-specific "is this pid alive" code is needed, and
//! a reused pid can never resurrect a dead entry.
//!
//! These files are ephemeral run state, like `state.toml`: a malformed one is pruned
//! once stale, never fatal. They carry the same identity the favourites file already
//! stores (host, port, account name) plus safe counters — never a credential, a
//! clipboard payload, or a pixel.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How often a session rewrites its presence file.
pub const WRITE_INTERVAL: Duration = Duration::from_secs(2);

/// A file whose `updated_unix` is older than this is a dead session's leftovers:
/// five missed write intervals, generous enough that a briefly wedged machine does
/// not flicker sessions out of the list.
pub const STALE_AFTER: Duration = Duration::from_secs(10);

/// One live session's identity and safe counters, as written to its presence file.
///
/// Host, port, and account name are the same identity `favourites.toml` already
/// keeps in this directory; the counters are the safe subset of
/// [`crate::stats::SessionStats`]. No credential, path, payload, or pixel field
/// exists in this struct, so serialising it cannot leak one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPresence {
    pub pid: u32,
    /// The display name the window title uses (favourite name or bare host).
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Current session resolution (tracks dynamic resize).
    pub width: u16,
    pub height: u16,
    pub started_unix: u64,
    pub updated_unix: u64,
    pub frames: u64,
    pub bytes_in: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    /// Recent input round-trip p50, microseconds.
    pub latency_p50_us: Option<u32>,
    /// Recent wall-clock gap between paints p50, microseconds — the received frame
    /// cadence, idle stretches included.
    pub frame_gap_p50_us: Option<u32>,
    pub decode_errors: u64,
    /// Frames seen per codec, e.g. `{"ClearCodec": 1234}`.
    pub codecs: BTreeMap<String, u64>,
}

/// Seconds since the Unix epoch, saturating at 0 on a clock set before 1970.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `<config dir>/sessions`, next to `favourites.toml`. `None` when there is no
/// config directory at all — then presence is silently off, like window state.
pub fn default_dir() -> Option<PathBuf> {
    crate::favourites::Favourites::default_path()
        .ok()
        .map(|p| p.with_file_name("sessions"))
}

fn file_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.json"))
}

/// Write one presence file atomically (temp-and-rename, like `state.toml`), so a
/// concurrent `--sessions` can never read a half-written document.
pub fn write_to(dir: &Path, presence: &SessionPresence) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(presence).map_err(io::Error::other)?;
    let tmp = dir.join(format!(".{}.json.tmp", presence.pid));
    std::fs::write(&tmp, json)?;
    let renamed = std::fs::rename(&tmp, file_path(dir, presence.pid));
    if renamed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    renamed
}

/// Best-effort removal of this process's presence file; a failure just leaves a
/// file that goes stale and is pruned by the next lister.
pub fn remove_from(dir: &Path, pid: u32) {
    let _ = std::fs::remove_file(file_path(dir, pid));
}

/// Read every fresh presence file under `dir`, oldest session first.
///
/// Stale files (dead sessions) are pruned while here, so the directory never
/// accumulates leftovers. A malformed file is skipped, and removed once its
/// modification time says it is stale too — it is run-state cache, not user data.
pub fn list_from(dir: &Path, now_unix: u64) -> Vec<SessionPresence> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut live = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<SessionPresence>(&text).ok());
        match parsed {
            Some(p) if now_unix.saturating_sub(p.updated_unix) <= STALE_AFTER.as_secs() => {
                live.push(p);
            }
            Some(_) => {
                // Its writer stopped: the session is gone.
                let _ = std::fs::remove_file(&path);
            }
            None => {
                // Unreadable. Prune only once its mtime proves no live writer owns it.
                let stale_by_mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > STALE_AFTER);
                if stale_by_mtime {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    live.sort_by_key(|p| (p.started_unix, p.pid));
    live
}

/// Keep this session's presence file alive from a background thread.
///
/// `sample` is called every [`WRITE_INTERVAL`] to produce the current counters.
/// Setting `stop` ends the thread and removes the file — the session-end path does
/// both itself as well, because on macOS Cmd+Q exits the process without unwinding
/// and this thread would never see the flag.
pub fn spawn_writer(
    dir: PathBuf,
    stop: Arc<AtomicBool>,
    mut sample: impl FnMut() -> SessionPresence + Send + 'static,
) {
    std::thread::Builder::new()
        .name("presence".to_owned())
        .spawn(move || {
            let pid = std::process::id();
            loop {
                if stop.load(Ordering::Relaxed) {
                    remove_from(&dir, pid);
                    return;
                }
                if let Err(e) = write_to(&dir, &sample()) {
                    // One warning would repeat forever; a missing presence file only
                    // costs a listing, so give up quietly.
                    eprintln!("warning: session presence not recorded ({e})");
                    return;
                }
                // Sleep in short steps so a stop is honoured promptly.
                let deadline = std::time::Instant::now() + WRITE_INTERVAL;
                while std::time::Instant::now() < deadline {
                    if stop.load(Ordering::Relaxed) {
                        remove_from(&dir, pid);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })
        .map(drop)
        .unwrap_or_else(|e| eprintln!("warning: session presence not recorded ({e})"));
}

/// `2h13m`, `5m12s`, `42s`.
fn fmt_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// `512 B`, `1.2 KB`, `3.4 MB`, `1.2 GB`.
fn fmt_bytes(bytes: u64) -> String {
    const UNITS: &[(u64, &str)] = &[(1 << 30, "GB"), (1 << 20, "MB"), (1 << 10, "KB")];
    for &(scale, unit) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} B")
}

/// One rendered cell: its text, plus the SGR parameters to wrap it in when the output
/// is coloured. An empty code means the terminal's default.
struct Cell {
    text: String,
    sgr: &'static str,
}

impl Cell {
    fn new(text: impl Into<String>, sgr: &'static str) -> Self {
        Cell {
            text: text.into(),
            sgr,
        }
    }
}

/// Identity columns borrow the `--help` screen's palette so the two screens read as one
/// tool: bold for the name you typed, cyan for the address, dim for the bookkeeping.
const IDENTITY: &str = "1";
const ADDRESS: &str = "36";
const SECONDARY: &str = "2";
const HEADING: &str = "1;33";

/// The three grades a health column can land in.
const GOOD: &str = "32";
const WARN: &str = "33";
const BAD: &str = "1;31";
/// Never measured. Not a verdict, so it stays quiet rather than claiming green.
const UNMEASURED: &str = "2";

/// Grade a measurement where **higher is better** (fps, cache hit rate).
fn grade_high(value: f64, good: f64, warn: f64) -> &'static str {
    if value >= good {
        GOOD
    } else if value >= warn {
        WARN
    } else {
        BAD
    }
}

/// Grade a measurement where **lower is better** (input round-trip latency).
fn grade_low(value: f64, good: f64, warn: f64) -> &'static str {
    if value <= good {
        GOOD
    } else if value <= warn {
        WARN
    } else {
        BAD
    }
}

/// The columns one session renders to, in table order.
///
/// The four health columns — FPS, CACHE, P50, ERRS — carry a grade; the rest carry a
/// fixed identity colour. The thresholds are set from what this codebase has actually
/// measured: a remote session's encoder tops out near 30 fps, and a healthy input round
/// trip is tens of milliseconds (~30 ms to quench, ~48 ms to temper under AVC444) while
/// the non-AVC path's ~416 ms is the case a user calls "laggy".
fn row_cells(p: &SessionPresence, now_unix: u64) -> Vec<Cell> {
    let up = fmt_duration(now_unix.saturating_sub(p.started_unix));
    let (fps, fps_sgr) = match p.frame_gap_p50_us.filter(|&gap| gap > 0) {
        Some(gap) => {
            let fps = 1_000_000.0 / f64::from(gap);
            (
                format!("{}", fps.round() as u64),
                grade_high(fps, 20.0, 10.0),
            )
        }
        None => ("-".to_owned(), UNMEASURED),
    };
    let lookups = p.cache_hits + p.cache_misses;
    let (cache, cache_sgr) = if lookups > 0 {
        let hit_rate = (p.cache_hits as f64 / lookups as f64) * 100.0;
        (format!("{hit_rate:.0}%"), grade_high(hit_rate, 75.0, 40.0))
    } else {
        // An AVC444 session sends no cache PDUs at all, so a bare "-" is normal here.
        ("-".to_owned(), UNMEASURED)
    };
    let (p50, p50_sgr) = match p.latency_p50_us {
        Some(us) => {
            let ms = f64::from(us) / 1000.0;
            (format!("{ms:.1}ms"), grade_low(ms, 80.0, 200.0))
        }
        None => ("-".to_owned(), UNMEASURED),
    };
    // Zero decode errors is the expected state, not an achievement: stay quiet.
    let errs_sgr = if p.decode_errors > 0 { BAD } else { SECONDARY };
    vec![
        Cell::new(p.name.clone(), IDENTITY),
        Cell::new(format!("{}@{}:{}", p.user, p.host, p.port), ADDRESS),
        Cell::new(p.pid.to_string(), SECONDARY),
        Cell::new(up, ""),
        Cell::new(format!("{}x{}", p.width, p.height), ""),
        Cell::new(fps, fps_sgr),
        Cell::new(p.frames.to_string(), SECONDARY),
        Cell::new(fmt_bytes(p.bytes_in), SECONDARY),
        Cell::new(cache, cache_sgr),
        Cell::new(p50, p50_sgr),
        Cell::new(p.decode_errors.to_string(), errs_sgr),
    ]
}

const HEADER: &[&str] = &[
    "NAME", "TARGET", "PID", "UP", "RES", "FPS", "FRAMES", "RX", "CACHE", "P50", "ERRS",
];

/// Render the sessions table, one line per session, columns sized to the content.
///
/// The layout is identical with and without colour — widths come from the plain text
/// and the escapes wrap the already-padded cell — so the coloured form with its escapes
/// stripped is byte-for-byte the plain form a script parses. The caller decides what
/// "empty" prints, and (via [`crate::cli::stdout_wants_color`]) whether colour is wanted.
pub fn render_table(rows: &[SessionPresence], now_unix: u64, color: bool) -> String {
    let table: Vec<Vec<Cell>> = std::iter::once(
        HEADER
            .iter()
            .map(|h| Cell::new(h.to_string(), HEADING))
            .collect(),
    )
    .chain(rows.iter().map(|p| row_cells(p, now_unix)))
    .collect();
    let widths: Vec<usize> = (0..HEADER.len())
        .map(|col| {
            table
                .iter()
                .map(|row| row[col].text.len())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let last = HEADER.len() - 1;
    table
        .iter()
        .map(|row| {
            let line = row
                .iter()
                .zip(&widths)
                .enumerate()
                .map(|(col, (cell, w))| {
                    // The last column is never padded, so there is no trailing run of
                    // spaces to trim back off from underneath the escape sequences.
                    let padded = if col == last {
                        cell.text.clone()
                    } else {
                        format!("{:<w$}", cell.text)
                    };
                    crate::cli::sgr(&padded, cell.sgr, color)
                })
                .collect::<Vec<_>>()
                .join("  ");
            format!("{line}\n")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "mdrdp-presence-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&base).expect("create temp dir");
        base
    }

    /// Every field a different value, so a swapped pair cannot pass.
    fn presence(pid: u32, now: u64) -> SessionPresence {
        SessionPresence {
            pid,
            name: "quench".to_owned(),
            host: "quench.lan.example".to_owned(),
            port: 3390,
            user: "ano".to_owned(),
            width: 2560,
            height: 1440,
            started_unix: now - 133,
            updated_unix: now,
            frames: 4321,
            bytes_in: 1_300_000,
            cache_hits: 90,
            cache_misses: 10,
            latency_p50_us: Some(3_140),
            frame_gap_p50_us: Some(33_333),
            decode_errors: 7,
            codecs: BTreeMap::from([("ClearCodec".to_owned(), 4321)]),
        }
    }

    #[test]
    fn presence_round_trips_and_lists_while_fresh() {
        let dir = tmpdir();
        let now = unix_now();
        let written = presence(1111, now);
        write_to(&dir, &written).expect("write presence");

        let listed = list_from(&dir, now);
        assert_eq!(listed, vec![written]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_file_is_neither_listed_nor_left_behind() {
        let dir = tmpdir();
        let now = unix_now();
        let mut dead = presence(2222, now);
        dead.updated_unix = now - STALE_AFTER.as_secs() - 1;
        write_to(&dir, &dead).expect("write presence");

        assert!(list_from(&dir, now).is_empty());
        assert!(
            !file_path(&dir, 2222).exists(),
            "the lister prunes dead sessions' files"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exactly_at_the_staleness_boundary_still_counts_as_live() {
        let dir = tmpdir();
        let now = unix_now();
        let mut edge = presence(3333, now);
        edge.updated_unix = now - STALE_AFTER.as_secs();
        write_to(&dir, &edge).expect("write presence");
        assert_eq!(list_from(&dir, now).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_freshly_written_malformed_file_is_skipped_but_not_deleted() {
        let dir = tmpdir();
        let bad = dir.join("999.json");
        std::fs::write(&bad, "not { json").expect("write junk");

        assert!(list_from(&dir, unix_now()).is_empty());
        assert!(
            bad.exists(),
            "a fresh unreadable file may belong to a live writer mid-schema-change"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sessions_list_oldest_first() {
        let dir = tmpdir();
        let now = unix_now();
        let mut younger = presence(1, now);
        younger.started_unix = now - 5;
        let mut older = presence(2, now);
        older.started_unix = now - 500;
        write_to(&dir, &younger).expect("write");
        write_to(&dir, &older).expect("write");

        let pids: Vec<u32> = list_from(&dir, now).iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![2, 1]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_directory_lists_nothing() {
        assert!(list_from(Path::new("/nonexistent/mdrdp-presence"), unix_now()).is_empty());
    }

    #[test]
    fn remove_deletes_only_the_named_pid() {
        let dir = tmpdir();
        let now = unix_now();
        write_to(&dir, &presence(10, now)).expect("write");
        write_to(&dir, &presence(11, now)).expect("write");
        remove_from(&dir, 10);
        let pids: Vec<u32> = list_from(&dir, now).iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![11]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_table_renders_each_stat_in_its_own_column() {
        let now = unix_now();
        let text = render_table(&[presence(4242, now)], now, false);
        let mut lines = lines_of(&text);
        let header = lines.next().expect("header line");
        let row = lines.next().expect("one session row");
        assert!(lines.next().is_none(), "one session, one row");

        for label in [
            "NAME", "TARGET", "PID", "UP", "RES", "FPS", "FRAMES", "RX", "CACHE", "P50", "ERRS",
        ] {
            assert!(header.contains(label), "header is missing {label}");
        }
        let cells: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(
            cells,
            vec![
                "quench",
                "ano@quench.lan.example:3390",
                "4242",
                "2m13s",
                "2560x1440",
                "30", // 33,333 us between frames
                "4321",
                "1.2 MB",
                "90%", // 90 hits of 100 lookups
                "3.1ms",
                "7",
            ]
            .into_iter()
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn stats_that_never_happened_render_as_dashes_not_zeros() {
        let now = unix_now();
        let mut idle = presence(1, now);
        idle.latency_p50_us = None;
        idle.frame_gap_p50_us = None;
        idle.cache_hits = 0;
        idle.cache_misses = 0;
        let text = render_table(&[idle], now, false);
        let row = lines_of(&text).nth(1).expect("session row").to_owned();
        assert_eq!(
            row.split_whitespace().filter(|c| *c == "-").count(),
            3,
            "fps, cache and p50 must all show as unmeasured: {row}"
        );
    }

    #[test]
    fn durations_and_byte_counts_read_like_a_human_wrote_them() {
        assert_eq!(fmt_duration(42), "42s");
        assert_eq!(fmt_duration(5 * 60 + 12), "5m12s");
        assert_eq!(fmt_duration(2 * 3600 + 13 * 60), "2h13m");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1_300), "1.3 KB");
        assert_eq!(fmt_bytes(3_400_000), "3.2 MB");
        assert_eq!(fmt_bytes(1_300_000_000), "1.2 GB");
    }

    #[test]
    fn presence_json_carries_no_credential_shaped_field() {
        let json = serde_json::to_value(presence(1, unix_now())).expect("serialises");
        for forbidden in ["password", "secret", "credential", "clipboard", "pixels"] {
            assert!(
                json.get(forbidden).is_none(),
                "presence must not carry {forbidden}"
            );
        }
    }

    fn lines_of(text: &str) -> impl Iterator<Item = &str> {
        text.lines().filter(|l| !l.is_empty())
    }

    /// Every escape-wrapped run in one coloured line as `(sgr, text)`, padding trimmed.
    /// Cells left at the terminal default carry no escapes and so do not appear.
    fn coloured_runs(line: &str) -> Vec<(String, String)> {
        let mut runs = Vec::new();
        let mut rest = line;
        while let Some(start) = rest.find("\x1b[") {
            let after = &rest[start + 2..];
            let Some(m) = after.find('m') else { break };
            let (code, body) = (&after[..m], &after[m + 1..]);
            let end = body.find("\x1b[0m").unwrap_or(body.len());
            runs.push((code.to_owned(), body[..end].trim_end().to_owned()));
            // Step over the reset as well, or the next run's text is read as its code.
            rest = body[end..].strip_prefix("\x1b[0m").unwrap_or(&body[end..]);
        }
        runs
    }

    #[test]
    fn the_coloured_table_is_the_plain_table_underneath() {
        let now = unix_now();
        let rows = [presence(1, now), presence(22222, now)];
        assert_eq!(
            crate::cli::strip_ansi(&render_table(&rows, now, true)),
            render_table(&rows, now, false),
            "colour must not move a column or a character"
        );
        assert!(
            !render_table(&rows, now, false).contains('\x1b'),
            "the plain table is what a script parses"
        );
        assert!(render_table(&rows, now, true).contains('\x1b'));
    }

    #[test]
    fn identity_columns_reuse_the_help_screens_palette() {
        let now = unix_now();
        let text = render_table(&[presence(4242, now)], now, true);
        let mut lines = lines_of(&text);
        let header = coloured_runs(lines.next().expect("header"));
        assert!(
            header.iter().all(|(sgr, _)| sgr == "1;33"),
            "every header label wears the --help heading colour: {header:?}"
        );

        let row = coloured_runs(lines.next().expect("session row"));
        assert!(
            row.contains(&("1".to_owned(), "quench".to_owned())),
            "{row:?}"
        );
        assert!(
            row.contains(&("36".to_owned(), "ano@quench.lan.example:3390".to_owned())),
            "{row:?}"
        );
        assert!(
            row.contains(&("2".to_owned(), "4242".to_owned())),
            "{row:?}"
        );
    }

    #[test]
    fn health_columns_grade_the_measurement_not_the_column() {
        let now = unix_now();
        let mut healthy = presence(1, now);
        healthy.frame_gap_p50_us = Some(33_333); // 30 fps — the remote-session cap
        healthy.cache_hits = 95;
        healthy.cache_misses = 5;
        healthy.latency_p50_us = Some(30_000); // measured-normal typing round trip
        healthy.decode_errors = 0;

        let mut sick = presence(2, now);
        sick.frame_gap_p50_us = Some(500_000); // 2 fps
        sick.cache_hits = 1;
        sick.cache_misses = 9;
        sick.latency_p50_us = Some(416_000); // the non-AVC path's p50
        sick.decode_errors = 12;

        let text = render_table(&[healthy, sick], now, true);
        let mut lines = lines_of(&text).skip(1);
        let good = coloured_runs(lines.next().expect("healthy row"));
        let bad = coloured_runs(lines.next().expect("sick row"));

        for cell in ["30", "95%", "30.0ms"] {
            assert!(
                good.contains(&("32".to_owned(), cell.to_owned())),
                "{cell} should read as healthy: {good:?}"
            );
        }
        assert!(
            good.contains(&("2".to_owned(), "0".to_owned())),
            "no decode errors is the normal state, not an achievement: {good:?}"
        );
        for cell in ["2", "10%", "416.0ms", "12"] {
            assert!(
                bad.contains(&("1;31".to_owned(), cell.to_owned())),
                "{cell} should read as unhealthy: {bad:?}"
            );
        }
    }

    #[test]
    fn the_middle_band_warns_rather_than_condemning() {
        let now = unix_now();
        let mut middling = presence(1, now);
        middling.frame_gap_p50_us = Some(66_667); // 15 fps
        middling.cache_hits = 50;
        middling.cache_misses = 50;
        middling.latency_p50_us = Some(150_000); // 150 ms
        let text = render_table(&[middling], now, true);
        let row = coloured_runs(lines_of(&text).nth(1).expect("session row"));
        for cell in ["15", "50%", "150.0ms"] {
            assert!(
                row.contains(&("33".to_owned(), cell.to_owned())),
                "{cell} sits between good and bad: {row:?}"
            );
        }
    }

    #[test]
    fn an_unmeasured_stat_is_dim_rather_than_green() {
        let now = unix_now();
        let mut idle = presence(1, now);
        idle.latency_p50_us = None;
        idle.frame_gap_p50_us = None;
        idle.cache_hits = 0;
        idle.cache_misses = 0;
        let text = render_table(&[idle], now, true);
        let row = coloured_runs(lines_of(&text).nth(1).expect("session row"));
        assert_eq!(
            row.iter().filter(|(sgr, t)| sgr == "2" && t == "-").count(),
            3,
            "fps, cache and p50 are unknown, and unknown is not a pass: {row:?}"
        );
        assert!(
            !row.iter().any(|(sgr, t)| sgr == "32" && t == "-"),
            "an unmeasured stat must never read as good: {row:?}"
        );
    }

    #[test]
    fn the_grades_sit_on_the_boundary_they_claim() {
        assert_eq!(
            grade_high(20.0, 20.0, 10.0),
            GOOD,
            "the good bound is inclusive"
        );
        assert_eq!(grade_high(19.9, 20.0, 10.0), WARN);
        assert_eq!(
            grade_high(10.0, 20.0, 10.0),
            WARN,
            "the warn bound is inclusive"
        );
        assert_eq!(grade_high(9.9, 20.0, 10.0), BAD);
        assert_eq!(grade_low(80.0, 80.0, 200.0), GOOD);
        assert_eq!(grade_low(80.1, 80.0, 200.0), WARN);
        assert_eq!(grade_low(200.0, 80.0, 200.0), WARN);
        assert_eq!(grade_low(200.1, 80.0, 200.0), BAD);
    }
}
