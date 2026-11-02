//! Scheduling class for the threads a user waits on.
//!
//! macOS schedules by QoS class, not `nice`. A thread left at the default class
//! competes for performance cores with whatever else is runnable, so nice-19 batch
//! work (a cargo build, an emulator test run) still takes cores from the session and
//! input feels laggy while the network stays fast. Marking the UI thread and the
//! network/decode threads `QOS_CLASS_USER_INTERACTIVE` tells the scheduler they are
//! what the user is looking at.
//!
//! Call [`mark_latency_critical`] at the start of each such thread's body: the class
//! applies to the calling thread only. Elsewhere it is a no-op.

/// Give the calling thread the user-interactive scheduling class.
///
/// Returns whether the class was applied. A failure is harmless (the thread keeps
/// the default class), so callers may ignore the result.
pub fn mark_latency_critical() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::set_self(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE)
    }

    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use libc::qos_class_t;

    pub(super) fn set_self(class: qos_class_t) -> bool {
        // SAFETY: affects only the calling thread; no pointers are passed.
        let rc = unsafe { libc::pthread_set_qos_class_self_np(class, 0) };
        if rc != 0 {
            tracing::debug!(rc, "pthread_set_qos_class_self_np failed");
        }
        rc == 0
    }

    #[cfg(test)]
    /// The calling thread's QoS class as its raw `qos_class_t` value. Read as a
    /// `u32` so a class this libc's enum does not name cannot be undefined behaviour.
    pub(super) fn current() -> u32 {
        let mut raw: u32 = 0;
        // SAFETY: `pthread_self()` is the live calling thread, `raw` is a valid
        // out-pointer of the enum's `repr(u32)` size, and a null relative-priority
        // pointer is documented as allowed.
        let rc = unsafe {
            libc::pthread_get_qos_class_np(
                libc::pthread_self(),
                (&mut raw as *mut u32).cast::<qos_class_t>(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "pthread_get_qos_class_np failed");
        raw
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn marks_the_calling_thread_user_interactive() {
        let class = std::thread::spawn(|| {
            assert!(mark_latency_critical());
            macos::current()
        })
        .join()
        .expect("qos thread");
        assert_eq!(
            class,
            libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE as u32,
            "got raw QoS class {class:#x}"
        );
    }
}
