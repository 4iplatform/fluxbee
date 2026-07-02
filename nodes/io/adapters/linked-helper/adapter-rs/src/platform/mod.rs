//! Platform seam for the OS-specific self-update steps: swapping the running
//! binary and restarting into it. The OS-agnostic pipeline (download/verify)
//! lives in `self_update`. A Unix impl is provided; Windows is a stub until the
//! service-install track (phase 2) fills it in — confirmed feasible by the
//! Windows spike (see the update-loop runbook).

use std::path::{Path, PathBuf};

use crate::self_update::UpdateError;

pub trait PlatformOps {
    /// Atomically swaps in verified `new_bytes` for `current_exe`, retaining the
    /// prior binary at `self_update::prev_binary_path` for rollback. Returns the
    /// retained previous-binary path.
    fn swap_binary(&self, current_exe: &Path, new_bytes: &[u8]) -> Result<PathBuf, UpdateError>;

    /// Restarts into the (already swapped) binary at `current_exe`.
    ///
    /// When `supervised` (running under a service manager), exits the process so
    /// the manager starts a fresh one — this makes each boot countable by the
    /// boot-gate. Otherwise replaces the process image in place. On success this
    /// never returns; a returned value is the failure that blocked the restart.
    fn restart(&self, current_exe: &Path, supervised: bool) -> UpdateError;
}

#[cfg(unix)]
mod unix;
#[cfg(not(unix))]
mod windows;

#[cfg(unix)]
pub use unix::UnixPlatform as CurrentPlatform;
#[cfg(not(unix))]
pub use windows::WindowsPlatform as CurrentPlatform;

/// The platform implementation for the current target.
pub fn current() -> CurrentPlatform {
    CurrentPlatform
}

/// True when the adapter runs under a service manager (systemd sets
/// `INVOCATION_ID`; `FLUXBEE_LH_SUPERVISED` forces it on for tests). Under a
/// supervisor the update restart exits-and-is-restarted (enabling the boot-gate
/// rollback); otherwise it re-execs in place.
pub fn running_supervised() -> bool {
    std::env::var_os("INVOCATION_ID").is_some()
        || std::env::var_os("FLUXBEE_LH_SUPERVISED").is_some()
}
