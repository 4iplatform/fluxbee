//! Windows implementation of the platform seam — STUB (phase 2).
//!
//! Windows cannot overwrite/delete a running `.exe`, but it CAN rename it
//! (`MoveFileEx`), so the swap shape (rename current aside → move new in →
//! delete `.prev` on the next boot, once the old process has exited) is expected
//! to generalize. The restart must use spawn+exit / SCM restart instead of
//! `exec()`. The Windows spike (see the update-loop runbook) validates those two
//! assumptions before this is implemented.
//!
//! Until then both operations report `Unsupported`, so a Windows adapter keeps
//! running its current version rather than attempting an unimplemented swap.

use std::path::{Path, PathBuf};

use crate::platform::PlatformOps;
use crate::self_update::UpdateError;

pub struct WindowsPlatform;

impl PlatformOps for WindowsPlatform {
    fn swap_binary(&self, _current_exe: &Path, _new_bytes: &[u8]) -> Result<PathBuf, UpdateError> {
        Err(UpdateError::Unsupported(
            "self-update binary swap is not implemented on this platform yet (phase 2)".to_string(),
        ))
    }

    fn restart(&self, _current_exe: &Path, _supervised: bool) -> UpdateError {
        UpdateError::Unsupported(
            "self-update restart is not implemented on this platform yet (phase 2)".to_string(),
        )
    }
}
