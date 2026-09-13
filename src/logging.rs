//! Bounded diagnostic files for tracing output.
//!
//! A session can produce a large amount of useful AVC444 metadata, especially while a
//! patterned desktop is changing. Keep the latest history without allowing a forgotten
//! client to grow its log directory forever: two files alternate, and each is capped.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The filter used by an ordinary run when `MDRDP_LOG` is not set.
pub const DEFAULT_FILTER: &str =
    "ironrdp_egfx=trace,mdrdp::gfx=trace,mdrdp::surface=trace,mdrdp::window=trace";

/// The two diagnostic files are deliberately a little larger than a short smoke run.
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Stable basename for the two files in the client's config `logs` directory.
pub const DEFAULT_STEM: &str = "mdrdp-diagnostics";

const SLOT_NAMES: [&str; 2] = ["a", "b"];
const ROTATION_HEADER: &[u8] = b"--- mdrdp diagnostics rotated ---\n";

/// A pair of alternating, size-bounded files.
pub struct FlipFlopLog {
    directory: PathBuf,
    stem: String,
    max_bytes: u64,
    active_slot: usize,
    bytes: u64,
    file: File,
}

impl FlipFlopLog {
    /// Open the newest slot, or start with slot A. A full newest slot causes the next
    /// write to start in the other slot, truncating that slot before use.
    pub fn open(
        directory: impl AsRef<Path>,
        stem: impl Into<String>,
        max_bytes: u64,
    ) -> io::Result<Self> {
        if max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "diagnostic log size must be greater than zero",
            ));
        }

        let directory = directory.as_ref().to_owned();
        let stem = stem.into();
        fs::create_dir_all(&directory)?;
        let paths = [
            slot_path(&directory, &stem, 0),
            slot_path(&directory, &stem, 1),
        ];

        let newest = newest_slot(&paths)?;
        let (active_slot, file, bytes) = match newest {
            None => {
                let (file, bytes) = open_append(&paths[0])?;
                (0, file, bytes)
            }
            Some((slot, length, _)) if length >= max_bytes => {
                let next = (slot + 1) % SLOT_NAMES.len();
                let (file, bytes) = open_rotated(&paths[next], max_bytes)?;
                (next, file, bytes)
            }
            Some((slot, _, _)) => {
                let (file, bytes) = open_append(&paths[slot])?;
                (slot, file, bytes)
            }
        };

        // Keep the pair visible even during a short run that never reaches the cap.
        // The inactive file is empty until the first rotation, so this does not add
        // meaningful disk use or change which slot carries the current trace.
        for path in &paths {
            let _ = OpenOptions::new().create(true).append(true).open(path)?;
        }

        Ok(Self {
            directory,
            stem,
            max_bytes,
            active_slot,
            bytes,
            file,
        })
    }

    /// The stable paths in this pair, in slot order A then B.
    pub fn paths(&self) -> [PathBuf; 2] {
        [
            slot_path(&self.directory, &self.stem, 0),
            slot_path(&self.directory, &self.stem, 1),
        ]
    }

    fn refresh_size(&mut self) -> io::Result<()> {
        // Another mdrdp process may have rotated the shared pair. Reading the current
        // metadata before each write keeps this process from continuing with stale size
        // accounting after that truncation.
        self.bytes = self.file.metadata()?.len();
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        let next = (self.active_slot + 1) % SLOT_NAMES.len();
        let (file, bytes) = open_rotated(
            &slot_path(&self.directory, &self.stem, next),
            self.max_bytes,
        )?;
        self.file = file;
        self.bytes = bytes;
        self.active_slot = next;
        Ok(())
    }

    #[cfg(test)]
    fn active_slot(&self) -> usize {
        self.active_slot
    }
}

impl io::Write for FlipFlopLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut written = 0;
        while written < buf.len() {
            self.refresh_size()?;
            if self.bytes >= self.max_bytes {
                self.rotate()?;
            }

            let room = self.max_bytes - self.bytes;
            let chunk_len = room.min((buf.len() - written) as u64) as usize;
            if chunk_len == 0 {
                self.rotate()?;
                continue;
            }
            let count = self.file.write(&buf[written..written + chunk_len])?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "diagnostic log write made no progress",
                ));
            }
            self.bytes += count as u64;
            written += count;
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// The default `<config>/logs` directory used by detached sessions too.
pub fn default_log_directory() -> io::Result<PathBuf> {
    crate::favourites::Favourites::default_path()
        .map(|path| path.with_file_name("logs"))
        .map_err(|error| io::Error::new(io::ErrorKind::NotFound, error.to_string()))
}

/// Open the process-shared default diagnostics pair.
pub fn open_default() -> io::Result<FlipFlopLog> {
    FlipFlopLog::open(default_log_directory()?, DEFAULT_STEM, DEFAULT_MAX_BYTES)
}

fn slot_path(directory: &Path, stem: &str, slot: usize) -> PathBuf {
    directory.join(format!("{stem}-{}.log", SLOT_NAMES[slot]))
}

fn newest_slot(paths: &[PathBuf; 2]) -> io::Result<Option<(usize, u64, SystemTime)>> {
    let mut newest = None;
    for (slot, path) in paths.iter().enumerate() {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        let candidate = (slot, metadata.len(), modified);
        if newest
            .as_ref()
            .is_none_or(|(_, _, current): &(usize, u64, SystemTime)| modified > *current)
        {
            newest = Some(candidate);
        }
    }
    Ok(newest)
}

fn open_append(path: &Path) -> io::Result<(File, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let bytes = file.metadata()?.len();
    Ok((file, bytes))
}

fn open_rotated(path: &Path, max_bytes: u64) -> io::Result<(File, u64)> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    let bytes = if (ROTATION_HEADER.len() as u64) < max_bytes {
        file.write_all(ROTATION_HEADER)?;
        ROTATION_HEADER.len() as u64
    } else {
        0
    };
    file.flush()?;
    drop(file);
    let file = OpenOptions::new().append(true).open(path)?;
    Ok((file, bytes))
}

#[cfg(test)]
mod tests {
    use super::FlipFlopLog;
    use std::fs;
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("the clock is after the Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mdrdp-flip-flop-test-{}-{suffix}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.0.join("avc-a.log"));
            let _ = fs::remove_file(self.0.join("avc-b.log"));
            let _ = fs::remove_dir(&self.0);
        }
    }

    #[test]
    fn writer_flips_between_bounded_slots() {
        let directory = TestDirectory::new();
        let mut log = FlipFlopLog::open(&directory.0, "avc", 64).expect("open log");
        for slot in ['a', 'b'] {
            assert!(directory.0.join(format!("avc-{slot}.log")).exists());
        }

        log.write_all(b"0123456789012345678901234567890123456789012345678901234567890123")
            .expect("fill first slot");
        assert_eq!(log.active_slot(), 0);

        log.write_all(b"x").expect("rotate into second slot");
        assert_eq!(log.active_slot(), 1);
        log.write_all(b"01234567890123456789012345678")
            .expect("fill second slot");
        log.write_all(b"y").expect("rotate back into first slot");
        assert_eq!(log.active_slot(), 0);

        for slot in ['a', 'b'] {
            let length = fs::metadata(directory.0.join(format!("avc-{slot}.log")))
                .expect("slot exists")
                .len();
            assert!(length <= 64, "slot {slot} grew to {length} bytes");
        }
    }
}
