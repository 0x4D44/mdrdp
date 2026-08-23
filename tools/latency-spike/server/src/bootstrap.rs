//! Viewer-connect bootstrap state, kept portable so its edge cases are unit tested.

/// Tracks the connect edge until a frame has actually passed whole-frame admission.
#[derive(Debug, Default)]
pub(crate) struct ViewerBootstrap {
    was_connected: bool,
    pending: bool,
}

impl ViewerBootstrap {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Observe the current connection state. Returns true exactly on a connect edge.
    pub(crate) fn observe(&mut self, connected: bool) -> bool {
        if !connected {
            self.was_connected = false;
            self.pending = false;
            return false;
        }
        if self.was_connected {
            return false;
        }
        self.was_connected = true;
        self.pending = true;
        true
    }

    /// Whether an idle source poll should fall back to the retained desktop frame.
    pub(crate) fn use_retained_on_idle(
        &self,
        retained_available: bool,
        keyframe_requested: bool,
    ) -> bool {
        retained_available && (self.pending || keyframe_requested)
    }

    /// A source rebuild invalidates the retained pixels but not the viewer's need.
    pub(crate) fn source_recreated(&mut self) {
        self.pending = true;
    }

    /// Clear the need only after capacity checks admit the frame for encoding.
    pub(crate) fn frame_admitted(&mut self) {
        self.pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::ViewerBootstrap;

    #[test]
    fn reconnect_arms_one_retained_frame_after_an_idle_poll() {
        let mut bootstrap = ViewerBootstrap::new();

        assert!(bootstrap.observe(true));
        assert!(bootstrap.use_retained_on_idle(true, false));
        bootstrap.frame_admitted();
        assert!(!bootstrap.use_retained_on_idle(true, false));

        assert!(!bootstrap.observe(false));
        assert!(bootstrap.observe(true));
        assert!(bootstrap.use_retained_on_idle(true, false));
    }

    #[test]
    fn missing_or_invalidated_retained_frame_keeps_bootstrap_pending() {
        let mut bootstrap = ViewerBootstrap::new();

        assert!(bootstrap.observe(true));
        assert!(!bootstrap.use_retained_on_idle(false, false));
        bootstrap.source_recreated();
        assert!(!bootstrap.use_retained_on_idle(false, false));
        assert!(bootstrap.use_retained_on_idle(true, false));
    }

    #[test]
    fn a_fresh_admitted_frame_satisfies_bootstrap() {
        let mut bootstrap = ViewerBootstrap::new();

        assert!(bootstrap.observe(true));
        bootstrap.frame_admitted();
        assert!(!bootstrap.use_retained_on_idle(true, false));
    }

    #[test]
    fn a_recovery_keyframe_request_can_reuse_the_retained_frame() {
        let mut bootstrap = ViewerBootstrap::new();

        assert!(bootstrap.observe(true));
        bootstrap.frame_admitted();
        assert!(bootstrap.use_retained_on_idle(true, true));
        assert!(!bootstrap.use_retained_on_idle(false, true));
    }
}
