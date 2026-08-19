//! The native (rhydra) transport: SSH tunnel, probe, wire session.
//!
//! `mdrdp <host> --native` speaks the spike's wire protocol to a rhydra host the
//! user has deployed to, in place of IronRDP. SSH is the security boundary — the
//! host's listeners are loopback-only and reached through `-L` forwards owned by
//! this module. Design: `wrk_docs/2026.08.18 - HLD - rhydra tranche 3 - mdrdp
//! native MVP.md`.

pub mod deployed;
pub mod probe;
pub mod session;
pub mod ssh;

use crate::favourites::NativeMode;

/// Resolve the effective transport mode: flag beats favourite beats settings.
///
/// The `--native`/`--rdp` conflict is a usage error rejected at flag parsing,
/// before this runs. A favourite's `Auto` is indistinguishable from "unset"
/// (serde default), so a chosen favourite always speaks for itself and the
/// settings default applies only to bare hosts — the same rule
/// `settings.defaults.port` follows.
pub fn resolve_mode(
    cli_native: bool,
    cli_rdp: bool,
    favourite: Option<NativeMode>,
    settings_default: NativeMode,
) -> NativeMode {
    debug_assert!(!(cli_native && cli_rdp), "rejected at flag parsing");
    if cli_rdp {
        NativeMode::Never
    } else if cli_native {
        NativeMode::Always
    } else if let Some(mode) = favourite {
        mode
    } else {
        settings_default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_precedence_is_flag_favourite_settings() {
        use NativeMode::{Always, Auto, Never};
        // Flags beat everything, in both directions.
        assert_eq!(resolve_mode(true, false, Some(Never), Never), Always);
        assert_eq!(resolve_mode(false, true, Some(Always), Always), Never);
        // A chosen favourite speaks for itself, including its Auto default.
        assert_eq!(resolve_mode(false, false, Some(Never), Always), Never);
        assert_eq!(resolve_mode(false, false, Some(Auto), Never), Auto);
        // No favourite: the settings default decides; its default is Auto.
        assert_eq!(resolve_mode(false, false, None, Never), Never);
        assert_eq!(resolve_mode(false, false, None, Auto), Auto);
    }
}
