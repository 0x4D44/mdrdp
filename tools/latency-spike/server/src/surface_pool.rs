//! Portable lease bookkeeping for the converter's fixed GPU-surface budget.

use std::time::{Duration, Instant};

pub(crate) struct SaturationWatchdog {
    deadline: Duration,
    since: Option<Instant>,
}

impl SaturationWatchdog {
    pub(crate) fn new(deadline: Duration) -> Self {
        Self {
            deadline,
            since: None,
        }
    }

    pub(crate) fn expired(&mut self, saturated: bool, now: Instant) -> bool {
        if !saturated {
            self.since = None;
            return false;
        }
        let since = self.since.get_or_insert(now);
        now.saturating_duration_since(*since) >= self.deadline
    }
}

pub(crate) struct LeaseSlots {
    in_use: Vec<bool>,
    next: usize,
}

impl LeaseSlots {
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            in_use: vec![false; capacity],
            next: 0,
        }
    }

    pub(crate) fn available(&self) -> bool {
        self.in_use.iter().any(|in_use| !in_use)
    }

    pub(crate) fn acquire(&mut self) -> Option<usize> {
        if let Some(slot) = (0..self.in_use.len())
            .map(|offset| (self.next + offset) % self.in_use.len())
            .find(|&slot| !self.in_use[slot])
        {
            self.in_use[slot] = true;
            self.next = (slot + 1) % self.in_use.len();
            return Some(slot);
        }

        None
    }

    pub(crate) fn release(&mut self, slot: usize) -> Result<(), String> {
        let in_use = self
            .in_use
            .get_mut(slot)
            .ok_or_else(|| format!("encoder retired unknown NV12 surface {slot}"))?;
        if !*in_use {
            return Err(format!("encoder retired free NV12 surface {slot}"));
        }
        *in_use = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_a_lease_when_the_fixed_budget_is_full() {
        let mut slots = LeaseSlots::new(2);
        assert_eq!(slots.acquire(), Some(0));
        assert_eq!(slots.acquire(), Some(1));
        assert_eq!(slots.acquire(), None);
        assert!(!slots.available());
    }

    #[test]
    fn released_slots_are_reused_round_robin() {
        let mut slots = LeaseSlots::new(3);
        assert_eq!(slots.acquire(), Some(0));
        assert_eq!(slots.acquire(), Some(1));
        slots.release(0).unwrap();
        assert_eq!(slots.acquire(), Some(2));
        assert_eq!(slots.acquire(), Some(0));
    }

    #[test]
    fn rejects_unknown_and_duplicate_releases() {
        let mut slots = LeaseSlots::new(1);
        assert!(slots.release(1).unwrap_err().contains("unknown"));
        assert_eq!(slots.acquire(), Some(0));
        slots.release(0).unwrap();
        assert!(slots.release(0).unwrap_err().contains("free"));
    }

    #[test]
    fn persistent_saturation_expires_but_recovered_capacity_resets_it() {
        let start = Instant::now();
        let mut watchdog = SaturationWatchdog::new(Duration::from_millis(500));
        assert!(!watchdog.expired(true, start));
        assert!(!watchdog.expired(true, start + Duration::from_millis(499)));
        assert!(!watchdog.expired(false, start + Duration::from_millis(499)));
        assert!(!watchdog.expired(true, start + Duration::from_secs(10)));
        assert!(watchdog.expired(true, start + Duration::from_millis(10_500)));
    }
}
