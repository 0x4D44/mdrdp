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

/// The columns one session renders to, in table order.
fn row_cells(p: &SessionPresence, now_unix: u64) -> Vec<String> {
    let up = fmt_duration(now_unix.saturating_sub(p.started_unix));
    let fps = p
        .frame_gap_p50_us
        .filter(|&gap| gap > 0)
        .map(|gap| format!("{}", (1_000_000.0 / f64::from(gap)).round() as u64))
        .unwrap_or_else(|| "-".to_owned());
    let lookups = p.cache_hits + p.cache_misses;
    let cache = if lookups > 0 {
        format!("{:.0}%", (p.cache_hits as f64 / lookups as f64) * 100.0)
    } else {
        "-".to_owned()
    };
    let p50 = p
        .latency_p50_us
        .map(|us| format!("{:.1}ms", f64::from(us) / 1000.0))
        .unwrap_or_else(|| "-".to_owned());
    vec![
        p.name.clone(),
        format!("{}@{}:{}", p.user, p.host, p.port),
        p.pid.to_string(),
        up,
        format!("{}x{}", p.width, p.height),
        fps,
        p.frames.to_string(),
        fmt_bytes(p.bytes_in),
        cache,
        p50,
        p.decode_errors.to_string(),
    ]
}

const HEADER: &[&str] = &[
    "NAME", "TARGET", "PID", "UP", "RES", "FPS", "FRAMES", "RX", "CACHE", "P50", "ERRS",
];

/// Render the sessions table, one line per session, columns sized to the content.
/// Plain text, stdout-parseable — the caller decides what "empty" prints.
pub fn render_table(rows: &[SessionPresence], now_unix: u64) -> String {
    let table: Vec<Vec<String>> = std::iter::once(HEADER.iter().map(|h| h.to_string()).collect())
        .chain(rows.iter().map(|p| row_cells(p, now_unix)))
        .collect();
    let widths: Vec<usize> = (0..HEADER.len())
        .map(|col| table.iter().map(|row| row[col].len()).max().unwrap_or(0))
        .collect();
    table
        .iter()
        .map(|row| {
            let line = row
                .iter()
                .zip(&widths)
                .map(|(cell, w)| format!("{cell:<w$}"))
                .collect::<Vec<_>>()
                .join("  ");
            format!("{}\n", line.trim_end())
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
        let text = render_table(&[presence(4242, now)], now);
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
        let text = render_table(&[idle], now);
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
}
