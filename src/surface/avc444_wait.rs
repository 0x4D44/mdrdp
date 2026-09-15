//! Bounded spatial timing observations, independent of presentation acknowledgements.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::{Rect, coalesce_rects, subtract_rect};
use tracing::trace;

pub(super) const INITIAL_WAIT: Duration = Duration::from_millis(50);
const MIN_WAIT: Duration = Duration::from_millis(16);
const MAX_WAIT: Duration = Duration::from_millis(150);
const MARGIN: Duration = Duration::from_millis(8);
const OBSERVATION_LIMIT: Duration = Duration::from_millis(500);
const HISTORY: usize = 32;
const MAX_PROBES: usize = 8;
const MAX_PIECES: usize = 256;
const MAX_INPUT: usize = 512;

#[derive(Debug)]
struct Probe {
    start: Instant,
    // Keep the original footprint: newer luma in an already-refined part also
    // makes the cohort ambiguous, not just luma touching its remaining slivers.
    original: Vec<Rect>,
    remaining: Vec<Rect>,
}

#[derive(Debug)]
pub(super) struct ChromaWait {
    probes: Vec<Probe>,
    samples: VecDeque<Duration>,
    wait: Duration,
    downward_credit: u8,
}

impl Default for ChromaWait {
    fn default() -> Self {
        Self {
            probes: Vec::new(),
            samples: VecDeque::new(),
            wait: INITIAL_WAIT,
            downward_credit: 0,
        }
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

impl ChromaWait {
    pub(super) fn delay(&self) -> Duration {
        self.wait
    }

    pub(super) fn invalidate(&mut self, reason: &'static str) {
        if !self.probes.is_empty() {
            trace!(
                reason,
                probes = self.probes.len(),
                "AVC444 timing probes dropped"
            );
            self.probes.clear();
        }
    }

    fn record(&mut self, gap: Duration) {
        if self.samples.len() == HISTORY {
            self.samples.pop_front();
        }
        self.samples.push_back(gap);
        let target = (*self.samples.iter().max().expect("sample just inserted") + MARGIN)
            .clamp(MIN_WAIT, MAX_WAIT);
        let previous = self.wait;
        if target >= self.wait {
            self.wait = target;
            self.downward_credit = 0;
        } else {
            self.downward_credit += 1;
            if self.downward_credit == 4 {
                self.wait = (self.wait - Duration::from_millis(1)).max(target);
                self.downward_credit = 0;
            }
        }
        trace!(
            gap_us = gap.as_micros() as u64,
            previous_wait_us = previous.as_micros() as u64,
            learned_wait_us = self.wait.as_micros() as u64,
            samples = self.samples.len(),
            "AVC444 chroma timing sample"
        );
    }

    /// Called with accepted, mapped, nonempty rectangles. These observations do
    /// not claim a protocol pair identity, and never decide which pixels to decode.
    pub(super) fn observe(&mut self, luma: &[Rect], chroma: &[Rect], now: Instant) {
        if luma.len().saturating_add(chroma.len()) > MAX_INPUT {
            self.invalidate("input_budget");
            return;
        }

        let mut collision = false;
        self.probes.retain(|probe| {
            if now.saturating_duration_since(probe.start) > OBSERVATION_LIMIT {
                trace!(reason = "expired", "AVC444 timing probe dropped");
                return false;
            }
            let ambiguous = luma
                .iter()
                .any(|&new| probe.original.iter().any(|&old| overlaps(new, old)));
            collision |= ambiguous;
            if ambiguous {
                trace!(reason = "overlapping_luma", "AVC444 timing probe dropped");
            }
            !ambiguous
        });

        // Count both footprints and residuals. Temporary subtraction storage is
        // checked before extending, so hostile fragmentation stays bounded too.
        let mut pieces: usize = self
            .probes
            .iter()
            .map(|probe| probe.original.len() + probe.remaining.len())
            .sum();
        let mut completed = Vec::new();
        self.probes.retain_mut(|probe| {
            let old_pieces = probe.original.len() + probe.remaining.len();
            let allowance = MAX_PIECES - (pieces - old_pieces) - probe.original.len();
            for &cover in chroma {
                let mut next = Vec::new();
                for &region in &probe.remaining {
                    let remainder = subtract_rect(region, cover);
                    if next.len() + remainder.len() > allowance {
                        pieces -= old_pieces;
                        trace!(reason = "fragmentation", "AVC444 timing probe dropped");
                        return false;
                    }
                    next.extend(remainder);
                }
                coalesce_rects(&mut next);
                probe.remaining = next;
            }
            pieces -= old_pieces;
            if probe.remaining.is_empty() {
                completed.push(now.saturating_duration_since(probe.start));
                false
            } else {
                pieces += probe.original.len() + probe.remaining.len();
                true
            }
        });
        for gap in completed {
            self.record(gap);
        }

        // Combined updates cannot contribute artificial zero-gap samples. Empty
        // chroma can also mean skipped LC0, so do not label this as certain LC1.
        if !luma.is_empty() && chroma.is_empty() && !collision {
            if self.probes.len() == MAX_PROBES || pieces + luma.len() * 2 > MAX_PIECES {
                trace!(
                    reason = "probe_budget",
                    probes = self.probes.len(),
                    "AVC444 timing probe refused"
                );
                return;
            }
            self.probes.push(Probe {
                start: now,
                original: luma.to_vec(),
                remaining: luma.to_vec(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn high_water_rises_immediately_and_falls_slowly_with_bounds() {
        let mut learner = ChromaWait::default();
        learner.record(ms(80));
        assert_eq!(learner.delay(), ms(88));
        for _ in 0..34 {
            learner.record(ms(10));
        }
        assert_eq!(learner.delay(), ms(88));
        learner.record(ms(10));
        assert_eq!(learner.delay(), ms(87));
        for _ in 0..400 {
            learner.record(ms(0));
        }
        assert_eq!(learner.delay(), ms(16));
        learner.record(ms(400));
        assert_eq!(learner.delay(), ms(150));
    }

    #[test]
    fn partial_and_disjoint_coverage_sample_only_completed_cohorts() {
        let mut learner = ChromaWait::default();
        let t = Instant::now();
        let a = Rect::new(0, 0, 2, 1);
        let b = Rect::new(4, 0, 6, 1);
        learner.observe(&[a], &[], t);
        learner.observe(&[b], &[], t + ms(10));
        learner.observe(&[], &[Rect::new(0, 0, 1, 1)], t + ms(60));
        assert!(learner.samples.is_empty());
        learner.observe(&[], &[Rect::new(1, 0, 2, 1)], t + ms(80));
        assert_eq!(learner.samples, VecDeque::from([ms(80)]));
        assert_eq!(learner.probes.len(), 1);
        learner.observe(&[], &[b], t + ms(100));
        assert_eq!(learner.samples, VecDeque::from([ms(80), ms(90)]));
    }

    #[test]
    fn overlapping_original_footprint_drops_probe_without_replacement() {
        let mut learner = ChromaWait::default();
        let t = Instant::now();
        let a = Rect::new(0, 0, 2, 1);
        let half = Rect::new(0, 0, 1, 1);
        learner.observe(&[a], &[], t);
        learner.observe(&[], &[half], t + ms(10));
        learner.observe(&[half], &[], t + ms(20));
        assert!(learner.probes.is_empty());
        learner.observe(&[], &[a], t + ms(80));
        assert!(learner.samples.is_empty());
    }

    #[test]
    fn combined_updates_do_not_open_probes_but_can_complete_older_ones() {
        let mut learner = ChromaWait::default();
        let t = Instant::now();
        let a = Rect::new(0, 0, 2, 1);
        let b = Rect::new(4, 0, 6, 1);
        learner.observe(&[a], &[a], t);
        assert!(learner.probes.is_empty());
        assert!(learner.samples.is_empty());
        learner.observe(&[a], &[], t + ms(10));
        learner.observe(&[b], &[a], t + ms(90));
        assert_eq!(learner.samples, VecDeque::from([ms(80)]));
        assert!(learner.probes.is_empty());
    }

    #[test]
    fn expired_and_invalidated_probes_do_not_train_or_reset_history() {
        let mut learner = ChromaWait::default();
        let t = Instant::now();
        let a = Rect::new(0, 0, 2, 1);
        learner.record(ms(100));
        learner.observe(&[a], &[], t);
        learner.observe(&[], &[a], t + ms(501));
        assert!(learner.probes.is_empty());
        assert_eq!(learner.samples, VecDeque::from([ms(100)]));
        learner.observe(&[a], &[], t + ms(600));
        learner.invalidate("test_reset");
        learner.observe(&[], &[a], t + ms(700));
        assert_eq!(learner.samples, VecDeque::from([ms(100)]));
        assert_eq!(learner.delay(), ms(108));
    }

    #[test]
    fn metadata_budgets_refuse_and_drop_without_samples() {
        let mut learner = ChromaWait::default();
        let t = Instant::now();
        for x in 0..9 {
            learner.observe(&[Rect::new(x * 2, 0, x * 2 + 1, 1)], &[], t);
        }
        assert_eq!(learner.probes.len(), 8);
        learner.observe(&vec![Rect::new(0, 0, 1, 1); 513], &[], t);
        assert!(learner.probes.is_empty());
        learner.observe(&vec![Rect::new(0, 0, 1, 1); 129], &[], t);
        assert!(learner.probes.is_empty());
        // 128 initial pieces fit, but a hole makes one residual split into four.
        let regions: Vec<_> = (0..64).map(|x| Rect::new(x * 4, 0, x * 4 + 3, 3)).collect();
        learner.observe(&regions, &[], t);
        let holes: Vec<_> = (0..64)
            .map(|x| Rect::new(x * 4 + 1, 1, x * 4 + 2, 2))
            .collect();
        learner.observe(&[], &holes, t + ms(80));
        assert!(learner.probes.is_empty());
        assert!(learner.samples.is_empty());
        assert_eq!(learner.delay(), ms(50));
    }
}
