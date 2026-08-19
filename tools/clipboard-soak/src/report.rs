//! What the soak concludes, and the rules it concludes it by.
//!
//! Separated from the driving so the judgements are testable without eight
//! hours of wall clock — every rule below decides whether a run counts, and a
//! rule that has never been exercised is not a rule.

use serde::Serialize;
use std::time::Duration;

/// A run shorter than this is a **provisional gate**, not a discharge.
///
/// AC8's requirement is "all-day", and the number exists so the label cannot be
/// argued about after the fact: eight hours is the bar, four is the floor below
/// which the run is not worth quoting at all.
pub const FULL_RUN: Duration = Duration::from_secs(8 * 60 * 60);

/// Below this a run is not evidence of anything.
pub const MINIMUM_RUN: Duration = Duration::from_secs(4 * 60 * 60);

/// Consecutive misses in one direction that mean the clipboard has wedged.
///
/// One miss is a slow lap or a poll boundary. **Two in a row in the same
/// direction is the failure this tranche exists to prevent**, and the run stops
/// there rather than averaging it away over the next six hours.
pub const WEDGE_AFTER_CONSECUTIVE_MISSES: u32 = 2;

/// Which way a transfer went. Direction matters on its own: a clipboard that
/// works one way and not the other is a different fault from one that is dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    MacToHost,
    HostToMac,
}

impl Direction {
    pub fn name(self) -> &'static str {
        match self {
            Direction::MacToHost => "mac->host",
            Direction::HostToMac => "host->mac",
        }
    }
}

/// What became of one transfer.
///
/// **"Not exercised" is a third state, and conflating it with a miss is how a
/// harness invents its own failures.** The first version of this returned
/// `None` for a direction it never actually drove, which counts as a
/// non-arrival — two cycles in and the run would have declared a wedge that
/// had not happened.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Arrived, with its latency in milliseconds.
    Arrived(f64),
    /// Sent and never seen. A miss.
    NeverArrived,
    /// Not driven this run. Counts as neither.
    NotExercised,
}

impl Outcome {
    pub fn latency_ms(self) -> Option<f64> {
        match self {
            Outcome::Arrived(ms) => Some(ms),
            _ => None,
        }
    }
    pub fn is_miss(self) -> bool {
        self == Outcome::NeverArrived
    }
}

/// One attempted transfer.
#[derive(Debug, Clone, Serialize)]
pub struct Attempt {
    pub direction: Direction,
    /// Seconds since the run started, so every line in the report is placeable.
    pub at_secs: f64,
    pub outcome: Outcome,
    /// Nonces are synthetic by construction, which is why the harness may
    /// record one: it is generated here and never read from a real clipboard.
    pub nonce: String,
}

/// Why a run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ending {
    /// Ran to its planned duration.
    Completed,
    /// Two consecutive misses in one direction.
    Wedged { direction: String },
    /// Something else took the host mid-run. **Void, not failed** — the run
    /// measured a session that was not ours, and reporting it as a failure
    /// would be as wrong as reporting it as a pass.
    Void { why: String },
    /// The session itself died.
    SessionEnded { why: String },
}

/// What one soak proved, if anything.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub ending: Ending,
    /// `true` only for a completed run of at least [`FULL_RUN`] with no misses.
    pub discharges_ac8: bool,
    /// A completed run between [`MINIMUM_RUN`] and [`FULL_RUN`] with no misses.
    pub provisional: bool,
    /// One line, for the human and for the board post.
    pub summary: String,
}

/// Decide what a finished run means.
///
/// Deliberately strict about the difference between "nothing went wrong" and
/// "this discharges the criterion": a four-hour clean run is genuinely useful
/// and is genuinely not eight hours, and the report has to say which.
pub fn judge(ending: &Ending, ran_for: Duration, misses: u32) -> Verdict {
    let hours = ran_for.as_secs_f64() / 3600.0;
    match ending {
        Ending::Void { why } => Verdict {
            ending: ending.clone(),
            discharges_ac8: false,
            provisional: false,
            summary: format!(
                "VOID after {hours:.2} h — {why}. The run measured a session that was not ours; \
                 it is neither a pass nor a failure."
            ),
        },
        Ending::Wedged { direction } => Verdict {
            ending: ending.clone(),
            discharges_ac8: false,
            provisional: false,
            summary: format!(
                "WEDGED after {hours:.2} h — {WEDGE_AFTER_CONSECUTIVE_MISSES} consecutive misses \
                 {direction}. This is the failure the tranche exists to prevent."
            ),
        },
        Ending::SessionEnded { why } => Verdict {
            ending: ending.clone(),
            discharges_ac8: false,
            provisional: false,
            summary: format!("FAILED after {hours:.2} h — the session ended: {why}"),
        },
        Ending::Completed if misses > 0 => Verdict {
            ending: ending.clone(),
            discharges_ac8: false,
            provisional: false,
            summary: format!(
                "FAILED — ran {hours:.2} h to completion but {misses} transfer(s) never arrived. \
                 A clipboard that loses one payload in eight hours is not fixed."
            ),
        },
        Ending::Completed if ran_for >= FULL_RUN => Verdict {
            ending: ending.clone(),
            discharges_ac8: true,
            provisional: false,
            summary: format!("PASS — {hours:.2} h, zero misses. Discharges AC8."),
        },
        Ending::Completed if ran_for >= MINIMUM_RUN => Verdict {
            ending: ending.clone(),
            discharges_ac8: false,
            provisional: true,
            summary: format!(
                "PROVISIONAL — {hours:.2} h, zero misses. Useful, and NOT a discharge of \
                 \"all-day\": AC8 wants {} h.",
                FULL_RUN.as_secs() / 3600
            ),
        },
        Ending::Completed => Verdict {
            ending: ending.clone(),
            discharges_ac8: false,
            provisional: false,
            summary: format!(
                "TOO SHORT — {hours:.2} h is below the {} h floor and proves nothing about a \
                 soak-class requirement.",
                MINIMUM_RUN.as_secs() / 3600
            ),
        },
    }
}

/// n / min / median / p95 / max over arrival latencies.
///
/// **A distribution, never a single number.** The repo has published a single
/// sample as a settled figure twice and been wrong both times; a soak that
/// reported only a mean would be the third.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Latencies {
    pub n: usize,
    pub min_ms: f64,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

pub fn latencies(samples: &[f64]) -> Option<Latencies> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("no NaN latencies"));
    Some(Latencies {
        n: sorted.len(),
        min_ms: sorted[0],
        median_ms: percentile(&sorted, 50.0),
        p95_ms: percentile(&sorted, 95.0),
        max_ms: sorted[sorted.len() - 1],
    })
}

/// Nearest-rank percentile, which needs no interpolation and cannot invent a
/// value that was never measured.
fn percentile(sorted: &[f64], pct: f64) -> f64 {
    let rank = (pct / 100.0 * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

/// Tracks consecutive misses per direction, which is what "wedged" means.
#[derive(Debug, Default)]
pub struct MissRun {
    mac_to_host: u32,
    host_to_mac: u32,
}

impl MissRun {
    /// Record an outcome; returns the wedge if this one crossed the line.
    ///
    /// A hit **resets** its own direction only. A clipboard working one way
    /// while the other is dead is exactly the case a shared counter would hide.
    ///
    /// A direction that was **not exercised** changes nothing — it is not
    /// evidence of health and it is certainly not a miss.
    pub fn note(&mut self, direction: Direction, outcome: Outcome) -> Option<Ending> {
        if outcome == Outcome::NotExercised {
            return None;
        }
        let counter = match direction {
            Direction::MacToHost => &mut self.mac_to_host,
            Direction::HostToMac => &mut self.host_to_mac,
        };
        if outcome.latency_ms().is_some() {
            *counter = 0;
            return None;
        }
        *counter += 1;
        if *counter >= WEDGE_AFTER_CONSECUTIVE_MISSES {
            return Some(Ending::Wedged {
                direction: direction.name().to_owned(),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hours(h: f64) -> Duration {
        Duration::from_secs_f64(h * 3600.0)
    }

    #[test]
    fn a_full_clean_run_discharges_and_a_four_hour_one_only_provisions() {
        let full = judge(&Ending::Completed, hours(8.0), 0);
        assert!(full.discharges_ac8);
        assert!(!full.provisional);

        // The distinction the criterion insists on, and the one most likely to
        // be blurred when a run is cut short at 2 a.m.
        let short = judge(&Ending::Completed, hours(4.5), 0);
        assert!(
            !short.discharges_ac8,
            "4.5 h must not discharge an all-day claim"
        );
        assert!(short.provisional);
        assert!(short.summary.contains("PROVISIONAL"));
        assert!(short.summary.contains("NOT a discharge"));
    }

    #[test]
    fn a_run_below_the_floor_proves_nothing_rather_than_provisioning() {
        let v = judge(&Ending::Completed, hours(1.0), 0);
        assert!(!v.discharges_ac8);
        assert!(!v.provisional, "an hour is not a provisional soak");
        assert!(v.summary.contains("TOO SHORT"));
    }

    #[test]
    fn one_miss_fails_a_full_run_rather_than_being_averaged_away() {
        // Eight clean hours and a single lost payload is not a pass. A summary
        // that led with "8.00 h" and buried the miss would read as one.
        let v = judge(&Ending::Completed, hours(8.0), 1);
        assert!(!v.discharges_ac8);
        assert!(!v.provisional);
        assert!(v.summary.starts_with("FAILED"));
        assert!(v.summary.contains("never arrived"));
    }

    #[test]
    fn a_void_run_is_neither_a_pass_nor_a_failure() {
        // Another agent connecting mid-run makes the measurement meaningless,
        // not negative. Reporting it as a failure would send someone hunting a
        // bug that is not there.
        let v = judge(
            &Ending::Void {
                why: "another viewer took the host".to_owned(),
            },
            hours(6.0),
            0,
        );
        assert!(!v.discharges_ac8);
        assert!(!v.provisional);
        assert!(v.summary.contains("VOID"));
        assert!(v.summary.contains("neither a pass nor a failure"));
    }

    #[test]
    fn two_consecutive_misses_in_one_direction_is_a_wedge() {
        let mut run = MissRun::default();
        assert_eq!(
            run.note(Direction::MacToHost, Outcome::NeverArrived),
            None,
            "one miss is not a wedge"
        );
        assert_eq!(
            run.note(Direction::MacToHost, Outcome::NeverArrived),
            Some(Ending::Wedged {
                direction: "mac->host".to_owned()
            })
        );
    }

    #[test]
    fn a_hit_resets_only_its_own_direction() {
        // The case a single shared counter would hide: one direction dead while
        // the other keeps working, which reads as a healthy clipboard.
        let mut run = MissRun::default();
        assert_eq!(run.note(Direction::MacToHost, Outcome::NeverArrived), None);
        assert_eq!(run.note(Direction::HostToMac, Outcome::Arrived(5.0)), None);
        assert_eq!(
            run.note(Direction::MacToHost, Outcome::NeverArrived),
            Some(Ending::Wedged {
                direction: "mac->host".to_owned()
            }),
            "a hit the other way must not clear this direction's run of misses"
        );
    }

    #[test]
    fn alternating_misses_and_hits_never_wedge() {
        let mut run = MissRun::default();
        for _ in 0..20 {
            assert_eq!(run.note(Direction::MacToHost, Outcome::NeverArrived), None);
            assert_eq!(run.note(Direction::MacToHost, Outcome::Arrived(5.0)), None);
        }
    }

    #[test]
    fn a_direction_that_was_never_driven_is_not_a_miss() {
        // The bug this test exists for: the harness recorded an undriven
        // direction as a non-arrival, so two cycles in it declared a wedge that
        // had not happened. A soak that invents its own failures is worse than
        // no soak, because someone then goes looking for the bug.
        let mut run = MissRun::default();
        for _ in 0..10 {
            assert_eq!(
                run.note(Direction::HostToMac, Outcome::NotExercised),
                None,
                "an undriven direction must never accumulate towards a wedge"
            );
        }
    }

    #[test]
    fn latencies_report_a_distribution_and_invent_no_value() {
        let l = latencies(&[10.0, 20.0, 30.0, 40.0, 100.0]).expect("five samples");
        assert_eq!(l.n, 5);
        assert_eq!(l.min_ms, 10.0);
        assert_eq!(l.max_ms, 100.0);
        // Nearest-rank: every reported figure is a value that was measured.
        assert_eq!(l.median_ms, 30.0);
        assert_eq!(l.p95_ms, 100.0);
    }

    #[test]
    fn no_samples_yields_no_distribution_rather_than_zeroes() {
        // All-zero statistics look like a fast clipboard. They are the absence
        // of one.
        assert!(latencies(&[]).is_none());
    }
}
