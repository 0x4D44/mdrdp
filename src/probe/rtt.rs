//! TCP handshake round-trip sampling — the latency floor.
//!
//! Every input→pixel budget starts here: network floor, plus server encode, plus our
//! decode, plus present. A measurement in the tens of milliseconds on a LAN is dominated
//! by our own pipeline, not the wire.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The plan's pass condition for a usable baseline.
pub const REQUIRED_SAMPLES: usize = 1000;
pub const REQUIRED_DISTINCT_HOURS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    /// Wall-clock time of the sample, so batches collected across the day can be merged.
    pub unix_ms: u64,
    /// TCP handshake duration in microseconds.
    pub micros: u64,
}

/// Whether a merged sample set actually meets the baseline requirement.
///
/// Hours are counted in **UTC** — std carries no timezone database, and pulling one in
/// for a coverage heuristic is not worth a dependency. Reported as UTC so the number is
/// not mistaken for local time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Coverage {
    pub samples: usize,
    pub distinct_utc_hours: usize,
    pub required_samples: usize,
    pub required_distinct_hours: usize,
    pub meets_requirement: bool,
}

pub fn coverage(samples: &[Sample]) -> Coverage {
    let hours: BTreeSet<u64> = samples
        .iter()
        .map(|s| (s.unix_ms / 3_600_000) % 24)
        .collect();
    let n = samples.len();
    let h = hours.len();
    Coverage {
        samples: n,
        distinct_utc_hours: h,
        required_samples: REQUIRED_SAMPLES,
        required_distinct_hours: REQUIRED_DISTINCT_HOURS,
        meets_requirement: n >= REQUIRED_SAMPLES && h >= REQUIRED_DISTINCT_HOURS,
    }
}

pub fn resolve(host: &str, port: u16) -> io::Result<SocketAddr> {
    (host, port).to_socket_addrs()?.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no address for {host}:{port}"),
        )
    })
}

/// Time one TCP handshake. The connection is dropped immediately; we are measuring the
/// handshake, not the session.
pub fn connect_once(addr: &SocketAddr, timeout: Duration) -> io::Result<u64> {
    let start = Instant::now();
    let stream = TcpStream::connect_timeout(addr, timeout)?;
    let elapsed = start.elapsed();
    drop(stream);
    Ok(elapsed.as_micros() as u64)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Collect a batch of samples, pausing `interval` between them.
///
/// The pause is not politeness: back-to-back connects accumulate sockets in `TIME_WAIT`
/// on this end, which distorts later samples in the same batch.
pub fn sample_batch(
    addr: &SocketAddr,
    count: usize,
    interval: Duration,
    timeout: Duration,
) -> (Vec<Sample>, Vec<io::Error>) {
    let mut samples = Vec::with_capacity(count);
    let mut errors = Vec::new();

    for i in 0..count {
        match connect_once(addr, timeout) {
            Ok(micros) => samples.push(Sample {
                unix_ms: now_unix_ms(),
                micros,
            }),
            Err(e) => errors.push(e),
        }
        if i + 1 < count {
            thread::sleep(interval);
        }
    }

    (samples, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_hour(hour: u64, n: usize) -> Vec<Sample> {
        // A day boundary plus `hour` hours, so `(unix_ms / 3_600_000) % 24 == hour`.
        let base = hour * 3_600_000;
        (0..n)
            .map(|i| Sample {
                unix_ms: base + i as u64,
                micros: 4_000,
            })
            .collect()
    }

    #[test]
    fn coverage_counts_distinct_hours_not_samples() {
        let mut samples = at_hour(1, 500);
        samples.extend(at_hour(9, 500));
        samples.extend(at_hour(17, 500));

        let c = coverage(&samples);
        assert_eq!(c.samples, 1500);
        assert_eq!(c.distinct_utc_hours, 3);
        assert!(c.meets_requirement);
    }

    #[test]
    fn enough_samples_in_one_hour_does_not_meet_the_requirement() {
        // The failure this guards against: 5000 samples in one burst looking like a
        // baseline spread across the day.
        let c = coverage(&at_hour(3, 5000));
        assert_eq!(c.samples, 5000);
        assert_eq!(c.distinct_utc_hours, 1);
        assert!(!c.meets_requirement);
    }

    #[test]
    fn enough_hours_but_too_few_samples_does_not_meet_the_requirement() {
        let mut samples = at_hour(1, 10);
        samples.extend(at_hour(9, 10));
        samples.extend(at_hour(17, 10));
        let c = coverage(&samples);
        assert_eq!(c.distinct_utc_hours, 3);
        assert!(!c.meets_requirement);
    }

    #[test]
    fn same_hour_on_different_days_is_one_hour() {
        // 09:00 today and 09:00 tomorrow are the same time of day, not two.
        let mut samples = at_hour(9, 600);
        samples.extend(
            at_hour(9, 600)
                .into_iter()
                .map(|s| Sample {
                    unix_ms: s.unix_ms + 24 * 3_600_000,
                    ..s
                })
                .collect::<Vec<_>>(),
        );
        let c = coverage(&samples);
        assert_eq!(c.samples, 1200);
        assert_eq!(c.distinct_utc_hours, 1);
        assert!(!c.meets_requirement);
    }

    #[test]
    fn empty_input_meets_nothing() {
        let c = coverage(&[]);
        assert_eq!((c.samples, c.distinct_utc_hours), (0, 0));
        assert!(!c.meets_requirement);
    }

    #[test]
    fn samples_round_trip_through_json() {
        let s = Sample {
            unix_ms: 1_755_000_000_000,
            micros: 4_203,
        };
        let line = serde_json::to_string(&s).expect("serialise");
        let back: Sample = serde_json::from_str(&line).expect("deserialise");
        assert_eq!(s, back);
    }
}
