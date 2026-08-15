//! Small, redacted resource readings for the current process.
//!
//! [`snapshot`] performs no I/O other than the operating system calls needed to
//! read this process's accounting counters. It does not include a process ID,
//! command line, host name, path, or any other identifying data. Values are
//! cumulative since process start, except for resident memory, which is the
//! process peak reported by the operating system.

/// A point-in-time reading of current-process resource counters.
///
/// CPU fields are cumulative milliseconds of user and system CPU time. The
/// resident-memory field is the peak resident/working-set size in bytes. Each
/// field is optional because an OS counter can be unavailable or fail without
/// making the rest of an acceptance report unusable.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProcessSnapshot {
    pub user_cpu_ms: Option<u64>,
    pub system_cpu_ms: Option<u64>,
    pub peak_resident_bytes: Option<u64>,
}

/// Read cumulative CPU time and peak resident memory for this process.
pub fn snapshot() -> ProcessSnapshot {
    #[cfg(target_os = "macos")]
    {
        macos::snapshot()
    }

    #[cfg(target_os = "windows")]
    {
        windows::snapshot()
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        ProcessSnapshot::default()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::ProcessSnapshot;

    // These layouts match the macOS SDK's sys/resource.h on 64-bit macOS:
    // timeval { time_t (long), suseconds_t (int), 4-byte alignment padding }
    // followed by two timevals and fourteen long fields in rusage. The SDK
    // documents ru_maxrss as the maximum resident set size; macOS reports it
    // in bytes (unlike Linux's historical KiB convention).
    #[repr(C)]
    struct Timeval {
        seconds: i64,
        microseconds: i32,
        _padding: i32,
    }

    #[repr(C)]
    struct Rusage {
        user: Timeval,
        system: Timeval,
        max_resident: i64,
        _rest: [i64; 13],
    }

    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }

    const RUSAGE_SELF: i32 = 0;

    pub(super) fn snapshot() -> ProcessSnapshot {
        let mut usage = Rusage {
            user: Timeval {
                seconds: 0,
                microseconds: 0,
                _padding: 0,
            },
            system: Timeval {
                seconds: 0,
                microseconds: 0,
                _padding: 0,
            },
            max_resident: 0,
            _rest: [0; 13],
        };

        // getrusage returns zero on success and -1 on failure.
        if unsafe { getrusage(RUSAGE_SELF, &mut usage) } != 0 {
            return ProcessSnapshot::default();
        }

        ProcessSnapshot {
            user_cpu_ms: timeval_to_ms(&usage.user),
            system_cpu_ms: timeval_to_ms(&usage.system),
            peak_resident_bytes: usage.max_resident.try_into().ok(),
        }
    }

    fn timeval_to_ms(value: &Timeval) -> Option<u64> {
        if value.seconds < 0 || !(0..1_000_000).contains(&value.microseconds) {
            return None;
        }

        let seconds = u64::try_from(value.seconds).ok()?;
        seconds
            .checked_mul(1_000)?
            .checked_add(u64::try_from(value.microseconds).ok()? / 1_000)
    }

    #[cfg(test)]
    mod tests {
        use super::{Timeval, timeval_to_ms};

        #[test]
        fn timeval_conversion_uses_milliseconds_without_rounding_up() {
            assert_eq!(
                timeval_to_ms(&Timeval {
                    seconds: 12,
                    microseconds: 345_678,
                    _padding: 0,
                }),
                Some(12_345)
            );
        }

        #[test]
        fn invalid_timeval_is_unavailable() {
            assert_eq!(
                timeval_to_ms(&Timeval {
                    seconds: -1,
                    microseconds: 0,
                    _padding: 0,
                }),
                None
            );
            assert_eq!(
                timeval_to_ms(&Timeval {
                    seconds: 0,
                    microseconds: 1_000_000,
                    _padding: 0,
                }),
                None
            );
        }

        #[test]
        fn current_process_snapshot_has_sane_available_values() {
            let report = super::snapshot();
            assert!(report.user_cpu_ms.is_some());
            assert!(report.system_cpu_ms.is_some());
            assert!(report.peak_resident_bytes.is_some_and(|bytes| bytes > 0));
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::ProcessSnapshot;

    type Bool = i32;
    type Dword = u32;
    type Handle = *mut core::ffi::c_void;

    // These definitions match the Windows SDK FILETIME and
    // PROCESS_MEMORY_COUNTERS structures. PROCESS_MEMORY_COUNTERS uses SIZE_T
    // for its size fields, so usize is correct for both 32- and 64-bit builds.
    #[repr(C)]
    struct Filetime {
        low: Dword,
        high: Dword,
    }

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: Dword,
        page_fault_count: Dword,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn GetProcessTimes(
            process: Handle,
            creation_time: *mut Filetime,
            exit_time: *mut Filetime,
            kernel_time: *mut Filetime,
            user_time: *mut Filetime,
        ) -> Bool;
    }

    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: Handle,
            counters: *mut ProcessMemoryCounters,
            size: Dword,
        ) -> Bool;
    }

    pub(super) fn snapshot() -> ProcessSnapshot {
        let process = unsafe { GetCurrentProcess() };
        let (user_cpu_ms, system_cpu_ms) = process_times(process);
        let peak_resident_bytes = peak_working_set(process);

        ProcessSnapshot {
            user_cpu_ms,
            system_cpu_ms,
            peak_resident_bytes,
        }
    }

    fn process_times(process: Handle) -> (Option<u64>, Option<u64>) {
        let mut creation = Filetime { low: 0, high: 0 };
        let mut exit = Filetime { low: 0, high: 0 };
        let mut kernel = Filetime { low: 0, high: 0 };
        let mut user = Filetime { low: 0, high: 0 };

        if unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) }
            == 0
        {
            return (None, None);
        }

        (filetime_to_ms(&user), filetime_to_ms(&kernel))
    }

    fn peak_working_set(process: Handle) -> Option<u64> {
        let mut counters = ProcessMemoryCounters {
            cb: core::mem::size_of::<ProcessMemoryCounters>() as Dword,
            page_fault_count: 0,
            peak_working_set_size: 0,
            working_set_size: 0,
            quota_peak_paged_pool_usage: 0,
            quota_paged_pool_usage: 0,
            quota_peak_non_paged_pool_usage: 0,
            quota_non_paged_pool_usage: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
        };

        let success = unsafe {
            GetProcessMemoryInfo(
                process,
                &mut counters,
                core::mem::size_of::<ProcessMemoryCounters>() as Dword,
            )
        };
        (success != 0).then_some(counters.peak_working_set_size as u64)
    }

    fn filetime_to_ms(value: &Filetime) -> Option<u64> {
        let ticks = (u64::from(value.high) << 32) | u64::from(value.low);
        Some(ticks / 10_000)
    }

    #[cfg(test)]
    mod tests {
        use super::{Filetime, filetime_to_ms};

        #[test]
        fn filetime_conversion_uses_ten_thousand_100ns_ticks_per_ms() {
            assert_eq!(
                filetime_to_ms(&Filetime {
                    low: 12_345_678,
                    high: 0,
                }),
                Some(1_234)
            );
        }
    }
}
