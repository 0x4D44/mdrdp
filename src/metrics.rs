//! Redacted, machine-readable session evidence.
//!
//! [`SessionMetricsReport`] deliberately accepts only already-collected safe snapshots and
//! scalar session metadata. It has no host, user, credential, path, or payload fields, so
//! serialising it cannot accidentally turn an acceptance report into a session dump.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::audio::AudioStats;
use crate::gfx::GfxStats;
use crate::stats::{CacheStats, Latency, SessionStats};

/// Stable schema identifier for [`SessionMetricsReport`].
pub const SCHEMA_VERSION: u32 = 1;

/// Process-resource counters safe to include in an acceptance report.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ResourceMetrics {
    pub user_cpu_ms: u64,
    pub system_cpu_ms: u64,
    pub peak_resident_bytes: u64,
    pub average_cpu_percent: Option<f64>,
}

impl ResourceMetrics {
    /// Build a resource snapshot. CPU percentage is absent when elapsed time is zero.
    pub fn new(
        user_cpu_ms: u64,
        system_cpu_ms: u64,
        peak_resident_bytes: u64,
        elapsed_ms: u64,
    ) -> Self {
        let average_cpu_percent = (elapsed_ms > 0).then(|| {
            (user_cpu_ms.saturating_add(system_cpu_ms) as f64 / elapsed_ms as f64) * 100.0
        });
        Self {
            user_cpu_ms,
            system_cpu_ms,
            peak_resident_bytes,
            average_cpu_percent,
        }
    }
}

/// A complete latency snapshot, with every duration represented as integer microseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LatencySnapshot {
    pub sample_count: u64,
    pub min_us: Option<u32>,
    pub max_us: Option<u32>,
    pub recent_p50_us: Option<u32>,
    pub recent_p95_us: Option<u32>,
    pub recent_p99_us: Option<u32>,
    pub baseline_p50_us: Option<u32>,
    pub baseline_p95_us: Option<u32>,
    pub baseline_p99_us: Option<u32>,
    pub drift_us: Option<i64>,
}

impl From<&Latency> for LatencySnapshot {
    fn from(latency: &Latency) -> Self {
        let recent = latency.recent();
        let baseline = latency.baseline();
        Self {
            sample_count: latency.count(),
            min_us: latency.min(),
            max_us: (latency.count() > 0).then_some(latency.max()),
            recent_p50_us: recent.map(|p| p.p50),
            recent_p95_us: recent.map(|p| p.p95),
            recent_p99_us: recent.map(|p| p.p99),
            baseline_p50_us: baseline.map(|p| p.p50),
            baseline_p95_us: baseline.map(|p| p.p95),
            baseline_p99_us: baseline.map(|p| p.p99),
            drift_us: latency.drift_us(),
        }
    }
}

/// Safe session counters copied from [`SessionStats`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionMetrics {
    pub frames: u64,
    pub bytes_in: u64,
    pub cache: CacheStatsMetrics,
    pub codecs: BTreeMap<String, u64>,
    pub decode_errors: u64,
    pub undecoded_regions: u64,
}

impl From<&SessionStats> for SessionMetrics {
    fn from(stats: &SessionStats) -> Self {
        Self {
            frames: stats.frames,
            bytes_in: stats.bytes_in,
            cache: CacheStatsMetrics::from(&stats.cache),
            codecs: stats.codecs.clone(),
            decode_errors: stats.decode_errors,
            undecoded_regions: stats.undecoded_regions,
        }
    }
}

/// Safe bitmap-cache counters copied from [`CacheStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CacheStatsMetrics {
    pub hits: u64,
    pub misses: u64,
    pub entries: u64,
    pub evictions: u64,
    pub bytes_served: u64,
    pub bytes_stored: u64,
    pub bytes_from_wire: u64,
}

impl From<&CacheStats> for CacheStatsMetrics {
    fn from(cache: &CacheStats) -> Self {
        Self {
            hits: cache.hits,
            misses: cache.misses,
            entries: cache.entries,
            evictions: cache.evictions,
            bytes_served: cache.bytes_served,
            bytes_stored: cache.bytes_stored,
            bytes_from_wire: cache.bytes_from_wire,
        }
    }
}

/// Redacted JSON-ready evidence for one client session.
#[derive(Debug, Clone, Serialize)]
pub struct SessionMetricsReport {
    pub schema_version: u32,
    pub elapsed_ms: u64,
    pub end_state: String,
    pub latency: LatencySnapshot,
    pub session: SessionMetrics,
    pub gfx: GfxStats,
    pub audio: AudioStats,
    pub joined_channels: Vec<String>,
    pub resources: Option<ResourceMetrics>,
}

impl SessionMetricsReport {
    /// Build a report from safe point-in-time snapshots.
    ///
    /// `end_state` and `joined_channels` are caller-supplied labels only. No connection
    /// identity, credential, path, or protocol payload is accepted by this API.
    pub fn new<I, S>(
        elapsed_ms: u64,
        end_state: impl Into<String>,
        session: &SessionStats,
        gfx: &GfxStats,
        audio: &AudioStats,
        joined_channels: I,
        resources: Option<ResourceMetrics>,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            schema_version: SCHEMA_VERSION,
            elapsed_ms,
            end_state: end_state.into(),
            latency: LatencySnapshot::from(&session.latency),
            session: SessionMetrics::from(session),
            gfx: gfx.clone(),
            audio: audio.clone(),
            joined_channels: joined_channels.into_iter().map(Into::into).collect(),
            resources,
        }
    }
}

/// Write one complete, newline-terminated JSON report.
///
/// Serialization happens in memory before the destination is touched. A serialization
/// failure therefore cannot leave a plausible-looking partial JSON document on disk.
pub fn write_report(path: &Path, report: &SessionMetricsReport) -> std::io::Result<()> {
    let mut json = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    json.push(b'\n');
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn report_writer_emits_one_complete_newline_terminated_json_document() {
        let path = std::env::temp_dir().join(format!(
            "mdrdp-metrics-{}-{}.json",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let report = SessionMetricsReport::new(
            1,
            "graceful_shutdown",
            &SessionStats::new(),
            &GfxStats::default(),
            &AudioStats::default(),
            ["cliprdr"],
            None,
        );

        write_report(&path, &report).expect("write report");
        let bytes = std::fs::read(&path).expect("read report");
        assert_eq!(bytes.last(), Some(&b'\n'));
        let parsed: Value = serde_json::from_slice(&bytes).expect("complete JSON");
        assert_eq!(parsed["end_state"], "graceful_shutdown");
        std::fs::remove_file(path).expect("remove test report");
    }

    #[test]
    fn report_contains_exact_safe_snapshot_values() {
        let mut session = SessionStats::new();
        for sample in 1..=1_000 {
            session.latency.record(sample);
        }
        session.frames = 42;
        session.bytes_in = 1_024;
        session.cache = CacheStats {
            hits: 3,
            misses: 2,
            entries: 4,
            evictions: 5,
            bytes_served: 6,
            bytes_stored: 7,
            bytes_from_wire: 8,
        };
        session.codecs.insert("ClearCodec".into(), 9);
        session.decode_errors = 10;
        session.undecoded_regions = 11;

        let gfx = GfxStats {
            frames_completed: 12,
            surfaces_created: 13,
            ..GfxStats::default()
        };
        let audio = AudioStats {
            packets_received: 14,
            bytes_played: 15,
            ..AudioStats::default()
        };
        let report = SessionMetricsReport::new(
            16,
            "graceful_shutdown",
            &session,
            &gfx,
            &audio,
            ["cliprdr", "rdpsnd"],
            Some(ResourceMetrics::new(20, 10, 30, 16)),
        );

        assert_eq!(report.schema_version, 1);
        assert_eq!(report.elapsed_ms, 16);
        assert_eq!(report.latency.sample_count, 1_000);
        assert_eq!(report.latency.min_us, Some(1));
        assert_eq!(report.latency.max_us, Some(1_000));
        assert_eq!(report.latency.recent_p50_us, Some(744));
        assert_eq!(report.latency.recent_p95_us, Some(975));
        assert_eq!(report.latency.recent_p99_us, Some(995));
        assert_eq!(report.latency.baseline_p50_us, Some(50));
        assert_eq!(report.latency.baseline_p95_us, Some(95));
        assert_eq!(report.latency.baseline_p99_us, Some(99));
        assert_eq!(report.latency.drift_us, Some(694));
        assert_eq!(report.session.cache.bytes_from_wire, 8);
        assert_eq!(report.gfx.frames_completed, 12);
        assert_eq!(report.audio.bytes_played, 15);
        assert_eq!(report.resources.unwrap().average_cpu_percent, Some(187.5));

        let json = serde_json::to_value(report).expect("report serialises");
        assert_eq!(
            json["joined_channels"],
            serde_json::json!(["cliprdr", "rdpsnd"])
        );
    }

    #[test]
    fn report_json_is_redacted_by_api_shape() {
        let report = SessionMetricsReport::new(
            1,
            "graceful_shutdown",
            &SessionStats::new(),
            &GfxStats::default(),
            &AudioStats::default(),
            ["cliprdr"],
            None,
        );
        let json = serde_json::to_string(&report).expect("report serialises");
        let parsed: Value = serde_json::from_str(&json).expect("report is JSON");
        assert!(!json.contains("sentinel-host"));
        assert!(!json.contains("sentinel-user"));
        assert!(!json.contains("sentinel-secret"));
        assert!(parsed.get("host").is_none());
        assert!(parsed.get("user").is_none());
        assert!(parsed.get("password").is_none());
    }
}
