//! Portable scheduling rules for the bounded sender batch.

use std::time::Duration;

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
    use super::{flush_due, payload_first, BatchKind, STATS_FLUSH_INTERVAL};
    use std::time::Duration;

    #[derive(Debug, PartialEq, Eq)]
    struct Item(BatchKind, u8);

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
