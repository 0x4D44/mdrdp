//! Keeping the window the size the user chose, across display upheaval.
//!
//! An RDP session runs at a fixed resolution, so when the window shrinks the image is
//! scaled down to fit it — the session does not reflow. That makes an unasked-for resize
//! visible as a smaller, softer desktop rather than as a smaller window, and it is the
//! thing the user actually complains about after a night of the monitor being asleep.
//!
//! macOS and Windows both resize and move windows on the user's behalf when displays come
//! and go: a monitor powering down, a screen locking, a scale-factor change, a laptop
//! waking on a different set of screens. None of those are the user asking for a smaller
//! window, but they arrive as the same `Resized` event.
//!
//! This module is the policy that tells the two apart, kept free of winit types so it can
//! be tested without a display. It is deliberately conservative in both directions:
//!
//! * It only ever fights *shrinkage*. A window the system made bigger is left alone.
//! * It gives up after [`MAX_RESTORES`] attempts, so a window manager that disagrees with
//!   us wins. A client that re-asserts its geometry forever is worse than a small window.
//!
//! The user's intent survives giving up: `desired` is not overwritten by a size we were
//! fighting, so the next display event gets a fresh budget and another go.

/// A window size in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
}

impl Geometry {
    pub fn new(width: u32, height: u32) -> Self {
        Geometry { width, height }
    }

    /// Is `self` smaller than `other` in either axis?
    fn shrunk_from(&self, other: Geometry) -> bool {
        self.width < other.width || self.height < other.height
    }

    /// Shrink to fit a monitor, keeping each axis independently.
    ///
    /// Asking for a window larger than the screen is how you get a window manager to
    /// invent its own answer, which is exactly the fight we are trying to avoid.
    fn clamped_to(&self, monitor: Option<Geometry>) -> Geometry {
        match monitor {
            Some(m) => Geometry {
                width: self.width.min(m.width),
                height: self.height.min(m.height),
            },
            None => *self,
        }
    }
}

/// A resize arriving within this long after a display event is suspected to be the
/// system's doing rather than the user's.
///
/// Display reconfiguration is not instantaneous — macOS emits occlusion, scale and size
/// changes over a spread of some hundreds of milliseconds as displays settle.
pub const SETTLE_MS: u64 = 2_000;

/// How many times we will re-assert the geometry before accepting the system's answer.
pub const MAX_RESTORES: u32 = 3;

/// What a resize event turned out to be, once read in context.
///
/// The caller needs more than "argue or not": a resize judged to be the user's own act
/// is the one thing allowed to renegotiate the *session* resolution, so the policy —
/// the only component holding the context to tell — must say so explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeVerdict {
    /// The user chose this size; it has been adopted as the desired geometry.
    UserResize,
    /// The system imposed it; ask the window manager for this size back.
    Restore(Geometry),
    /// Leave it alone: growth, an occluded screen, a spent budget, or a restore
    /// landing. Never treat it as the user's intent.
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Nothing in flight; a resize now is taken at face value.
    Idle,
    /// We have asked for a size back and are waiting to see it. Resizes while restoring
    /// are never treated as the user's intent, however long the restore takes.
    Restoring(u32),
    /// We tried and the system disagreed. Stop fighting until the next display event.
    GaveUp,
}

/// Decides whether a resize should be accepted or argued with.
///
/// Feed it every resize, plus the display events that provide the context for reading
/// them, and it returns the size to request back — or `None`, meaning leave it alone.
#[derive(Debug)]
pub struct WindowPolicy {
    desired: Geometry,
    occluded: bool,
    phase: Phase,
    last_display_event_ms: Option<u64>,
}

impl WindowPolicy {
    /// Start from the size the window was created at — the user's initial intent.
    pub fn new(desired: Geometry) -> Self {
        WindowPolicy {
            desired,
            occluded: false,
            phase: Phase::Idle,
            last_display_event_ms: None,
        }
    }

    /// The size we are currently trying to hold.
    pub fn desired(&self) -> Geometry {
        self.desired
    }

    /// A display changed: scale factor, monitor set, or the window became visible again.
    ///
    /// This opens a window of suspicion around the resizes that follow, and restores the
    /// restore budget so a fresh upheaval always gets a fresh attempt.
    pub fn note_display_event(&mut self, now_ms: u64) {
        self.last_display_event_ms = Some(now_ms);
        self.phase = Phase::Idle;
    }

    /// The window was occluded or revealed — on macOS this is what a screen lock looks
    /// like, and it brackets the period during which geometry gets rearranged.
    ///
    /// Becoming visible counts as a display event: that is the moment we can act.
    pub fn note_occluded(&mut self, occluded: bool, now_ms: u64) {
        let was = self.occluded;
        self.occluded = occluded;
        if was && !occluded {
            self.note_display_event(now_ms);
        }
    }

    /// Consider a resize. Says whether it was the user's act, a system imposition to
    /// argue with, or nothing worth acting on.
    ///
    /// `monitor` is the current screen size where known; a restore is clamped to it.
    pub fn on_resize(
        &mut self,
        actual: Geometry,
        now_ms: u64,
        monitor: Option<Geometry>,
    ) -> ResizeVerdict {
        // We got what we wanted. Whatever we were doing, we are done doing it. Not a
        // user act even in a quiet period: nothing changed.
        if actual == self.desired {
            self.phase = Phase::Idle;
            return ResizeVerdict::Ignore;
        }

        // Nothing can be fixed on a screen that is off, and trying burns the budget we
        // will want the moment it comes back. Remember, act later.
        if self.occluded {
            return ResizeVerdict::Ignore;
        }

        // A restore in flight keeps the resize suspect however long the round trip takes.
        // Without this, a slow window manager would outlast the settle window and we
        // would adopt the very size we were arguing with as the user's intent.
        let suspect = matches!(self.phase, Phase::Restoring(_)) || self.within_settle(now_ms);

        if !suspect {
            // Quiet period, no restore pending: the user resized the window. That is now
            // what they want, including if it is smaller.
            self.desired = actual;
            self.phase = Phase::Idle;
            return ResizeVerdict::UserResize;
        }

        // Only shrinkage is worth fighting. A window the system made larger still shows
        // the whole session, so leave it be.
        if !actual.shrunk_from(self.desired) {
            return ResizeVerdict::Ignore;
        }

        if self.phase == Phase::GaveUp {
            return ResizeVerdict::Ignore;
        }

        let attempts = match self.phase {
            Phase::Restoring(n) => n,
            _ => 0,
        };
        if attempts >= MAX_RESTORES {
            // The system wins this round. `desired` is deliberately left untouched so the
            // next display event can try again for the size the user actually asked for.
            self.phase = Phase::GaveUp;
            return ResizeVerdict::Ignore;
        }

        let target = self.desired.clamped_to(monitor);
        if target == actual {
            // The screen cannot hold any more than we already have. Not a fight worth
            // having, and re-requesting the same size would spin.
            self.phase = Phase::GaveUp;
            return ResizeVerdict::Ignore;
        }

        self.phase = Phase::Restoring(attempts + 1);
        ResizeVerdict::Restore(target)
    }

    fn within_settle(&self, now_ms: u64) -> bool {
        self.last_display_event_ms
            .is_some_and(|t| now_ms.saturating_sub(t) <= SETTLE_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIG: Geometry = Geometry {
        width: 2560,
        height: 1440,
    };
    const SMALL: Geometry = Geometry {
        width: 640,
        height: 480,
    };

    fn policy() -> WindowPolicy {
        WindowPolicy::new(BIG)
    }

    #[test]
    fn a_resize_in_a_quiet_period_is_the_user_and_is_adopted() {
        let mut p = policy();
        // No display event has ever happened, so nothing is suspect.
        assert_eq!(p.on_resize(SMALL, 10_000, None), ResizeVerdict::UserResize);
        assert_eq!(p.desired(), SMALL, "a user resize becomes the new intent");
    }

    #[test]
    fn a_shrink_just_after_a_display_event_is_argued_with() {
        let mut p = policy();
        p.note_display_event(1_000);
        assert_eq!(
            p.on_resize(SMALL, 1_100, None),
            ResizeVerdict::Restore(BIG),
            "the system shrank us; ask for the size back"
        );
        assert_eq!(p.desired(), BIG, "the user's intent is not overwritten");
    }

    #[test]
    fn a_shrink_long_after_a_display_event_is_the_user() {
        let mut p = policy();
        p.note_display_event(1_000);
        // Well outside the settle window: this is the user dragging the window.
        assert_eq!(
            p.on_resize(SMALL, 1_000 + SETTLE_MS + 1, None),
            ResizeVerdict::UserResize
        );
        assert_eq!(p.desired(), SMALL);
    }

    #[test]
    fn nothing_is_restored_while_occluded_but_it_is_restored_on_return() {
        let mut p = policy();
        p.note_occluded(true, 1_000);
        assert_eq!(
            p.on_resize(SMALL, 1_100, None),
            ResizeVerdict::Ignore,
            "a screen that is off cannot be fixed"
        );
        assert_eq!(p.desired(), BIG, "and the intent must survive the lock");

        p.note_occluded(false, 50_000);
        assert_eq!(
            p.on_resize(SMALL, 50_010, None),
            ResizeVerdict::Restore(BIG),
            "now that we can see, put it back"
        );
    }

    #[test]
    fn growth_during_upheaval_is_neither_fought_nor_the_user() {
        let mut p = policy();
        p.note_display_event(1_000);
        let bigger = Geometry::new(3840, 2160);
        assert_eq!(p.on_resize(bigger, 1_100, None), ResizeVerdict::Ignore);
    }

    #[test]
    fn growth_in_a_quiet_period_is_the_user() {
        let mut p = policy();
        let bigger = Geometry::new(3840, 2160);
        assert_eq!(p.on_resize(bigger, 10_000, None), ResizeVerdict::UserResize);
        assert_eq!(p.desired(), bigger);
    }

    #[test]
    fn restores_are_bounded_so_we_never_fight_forever() {
        let mut p = policy();
        p.note_display_event(1_000);
        for attempt in 1..=MAX_RESTORES {
            assert_eq!(
                p.on_resize(SMALL, 1_000 + u64::from(attempt) * 10, None),
                ResizeVerdict::Restore(BIG),
                "attempt {attempt} should still be trying"
            );
        }
        assert_eq!(
            p.on_resize(SMALL, 9_999_999, None),
            ResizeVerdict::Ignore,
            "after {MAX_RESTORES} attempts the system wins, and giving up is not the user"
        );
        assert_eq!(
            p.desired(),
            BIG,
            "giving up must not discard what the user asked for"
        );
    }

    #[test]
    fn a_restore_in_flight_outlasts_the_settle_window() {
        let mut p = policy();
        p.note_display_event(1_000);
        assert_eq!(p.on_resize(SMALL, 1_100, None), ResizeVerdict::Restore(BIG));
        // The window manager takes its time — far longer than the settle window.
        let late = 1_000 + SETTLE_MS + 60_000;
        assert_eq!(
            p.on_resize(SMALL, late, None),
            ResizeVerdict::Restore(BIG),
            "still restoring, so this is not the user"
        );
        assert_eq!(
            p.desired(),
            BIG,
            "a slow window manager must not rewrite the user's intent"
        );
    }

    #[test]
    fn a_new_display_event_grants_a_fresh_budget() {
        let mut p = policy();
        p.note_display_event(1_000);
        for _ in 0..MAX_RESTORES {
            p.on_resize(SMALL, 1_100, None);
        }
        assert_eq!(
            p.on_resize(SMALL, 1_200, None),
            ResizeVerdict::Ignore,
            "budget spent"
        );

        p.note_display_event(500_000);
        assert_eq!(
            p.on_resize(SMALL, 500_100, None),
            ResizeVerdict::Restore(BIG),
            "a new upheaval deserves another go"
        );
    }

    #[test]
    fn the_request_is_clamped_to_the_monitor() {
        let mut p = policy();
        p.note_display_event(1_000);
        let monitor = Geometry::new(1920, 1080);
        assert_eq!(
            p.on_resize(SMALL, 1_100, Some(monitor)),
            ResizeVerdict::Restore(monitor),
            "never ask for a window bigger than the screen"
        );
    }

    #[test]
    fn a_window_already_filling_a_smaller_monitor_is_not_fought() {
        let mut p = policy();
        p.note_display_event(1_000);
        let monitor = Geometry::new(1280, 800);
        // We are already exactly the monitor size; asking again would spin.
        assert_eq!(
            p.on_resize(monitor, 1_100, Some(monitor)),
            ResizeVerdict::Ignore
        );
    }

    #[test]
    fn reaching_the_desired_size_settles_and_restores_the_budget() {
        let mut p = policy();
        p.note_display_event(1_000);
        assert_eq!(p.on_resize(SMALL, 1_100, None), ResizeVerdict::Restore(BIG));
        // The restore landed — that is the round trip completing, not the user acting.
        assert_eq!(p.on_resize(BIG, 1_200, None), ResizeVerdict::Ignore);
        // A later user resize in a quiet period is adopted, proving we left Restoring.
        assert_eq!(p.on_resize(SMALL, 900_000, None), ResizeVerdict::UserResize);
        assert_eq!(p.desired(), SMALL);
    }

    #[test]
    fn occlusion_without_a_prior_reveal_does_not_invent_a_display_event() {
        let mut p = policy();
        // Becoming occluded is not itself the moment to act.
        p.note_occluded(true, 1_000);
        p.note_occluded(true, 1_100);
        p.note_occluded(false, 1_200);
        // Now visible: the reveal is the display event, so this shrink is suspect.
        assert_eq!(p.on_resize(SMALL, 1_250, None), ResizeVerdict::Restore(BIG));
    }
}
