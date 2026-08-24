//! Portable scheduling rules for the bounded sender batch.

#[cfg(any(windows, test))]
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone)]
#[cfg(any(windows, test))]
pub struct AdmissionGate(Arc<(Mutex<Option<bool>>, Condvar)>);

#[cfg(any(windows, test))]
impl AdmissionGate {
    pub(crate) fn pending() -> Self {
        Self(Arc::new((Mutex::new(None), Condvar::new())))
    }

    pub(crate) fn decide(&self, admitted: bool) {
        let (state, changed) = &*self.0;
        *state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(admitted);
        changed.notify_all();
    }

    pub(crate) fn admitted(&self) -> bool {
        let (state, changed) = &*self.0;
        let mut decision = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while decision.is_none() {
            decision = changed
                .wait(decision)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        decision.unwrap_or(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchKind {
    Payload,
    Line,
}

/// Maximum dirty interval while the sender is making progress. A blocked socket can
/// hold the owning thread until its separate write timeout expires.
pub(crate) const STATS_FLUSH_INTERVAL: Duration = Duration::from_millis(200);

pub(crate) fn payload_first<T>(items: &mut [T], classify: impl Fn(&T) -> BatchKind) {
    // Stable: pixel payloads retain arrival order; stats move behind them.
    items.sort_by_key(|item| match classify(item) {
        BatchKind::Payload => 0,
        BatchKind::Line => 1,
    });
}

pub(crate) fn flush_due(dirty: bool, elapsed: Duration) -> bool {
    dirty && elapsed >= STATS_FLUSH_INTERVAL
}

#[cfg(test)]
mod tests {
    use super::{flush_due, payload_first, AdmissionGate, BatchKind, STATS_FLUSH_INTERVAL};
    use std::time::Duration;

    #[derive(Debug, PartialEq, Eq)]
    struct Item(BatchKind, u8);

    #[test]
    fn paired_sender_gate_publishes_only_after_one_shared_decision() {
        for decision in [false, true] {
            let gate = AdmissionGate::pending();
            let waiter = gate.clone();
            let joined = std::thread::spawn(move || waiter.admitted());
            gate.decide(decision);
            assert_eq!(joined.join().unwrap(), decision);
        }
    }

    #[test]
    fn payloads_are_delivered_before_stats_lines() {
        let mut actual = vec![
            Item(BatchKind::Payload, 1),
            Item(BatchKind::Line, 2),
            Item(BatchKind::Payload, 3),
            Item(BatchKind::Payload, 4),
            Item(BatchKind::Payload, 5),
            Item(BatchKind::Line, 6),
        ];
        payload_first(&mut actual, |item| item.0);

        assert_eq!(
            actual,
            vec![
                Item(BatchKind::Payload, 1),
                Item(BatchKind::Payload, 3),
                Item(BatchKind::Payload, 4),
                Item(BatchKind::Payload, 5),
                Item(BatchKind::Line, 2),
                Item(BatchKind::Line, 6),
            ]
        );
    }

    #[test]
    fn dirty_stats_flush_at_the_bounded_interval() {
        assert!(!flush_due(true, Duration::from_millis(199)));
        assert!(flush_due(true, STATS_FLUSH_INTERVAL));
        assert!(!flush_due(false, Duration::from_secs(1)));
    }
}
