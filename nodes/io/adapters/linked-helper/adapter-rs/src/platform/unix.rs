//! Unix implementation of the platform seam: rename-based atomic swap (a running
//! binary can be renamed and replaced on Unix) and in-place `exec()` restart,
//! with a clean exit under a supervisor so the boot-gate can count restarts.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use crate::platform::PlatformOps;
use crate::self_update::{prev_binary_path, temp_binary_path, UpdateError};

/// Exit code used for the supervised restart. `Restart=always` restarts on any
/// exit, so the specific value is informational.
const RESTART_EXIT_CODE: i32 = 0;

pub struct UnixPlatform;

impl PlatformOps for UnixPlatform {
    fn swap_binary(&self, current_exe: &Path, new_bytes: &[u8]) -> Result<PathBuf, UpdateError> {
        let temp = temp_binary_path(current_exe);
        let prev = prev_binary_path(current_exe);

        fs::write(&temp, new_bytes).map_err(|e| UpdateError::Swap(format!("write temp: {}", e)))?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o755))
            .map_err(|e| UpdateError::Swap(format!("chmod temp: {}", e)))?;

        let _ = fs::remove_file(&prev);
        fs::rename(current_exe, &prev)
            .map_err(|e| UpdateError::Swap(format!("move current aside: {}", e)))?;

        if let Err(e) = fs::rename(&temp, current_exe) {
            // Roll back: restore the previous binary to its original path.
            let _ = fs::rename(&prev, current_exe);
            let _ = fs::remove_file(&temp);
            return Err(UpdateError::Swap(format!("install new binary: {}", e)));
        }

        Ok(prev)
    }

    fn restart(&self, current_exe: &Path, supervised: bool) -> UpdateError {
        if supervised {
            // Let the service manager start a fresh process (boot-gate counts it).
            eprintln!("self-update: exiting for supervised restart into the new binary");
            std::process::exit(RESTART_EXIT_CODE);
        }
        let args: Vec<String> = std::env::args().skip(1).collect();
        let error = std::process::Command::new(current_exe).args(args).exec();
        UpdateError::Swap(format!("re-exec failed: {}", error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_binary_installs_new_and_retains_prev() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("lh-swap-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let exe = dir.join("adapter-rs");
        {
            let mut f = fs::File::create(&exe).unwrap();
            f.write_all(b"OLD-BINARY").unwrap();
        }

        let prev = UnixPlatform.swap_binary(&exe, b"NEW-BINARY").unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"NEW-BINARY");
        assert_eq!(fs::read(&prev).unwrap(), b"OLD-BINARY");

        // rollback restores the old bytes
        crate::self_update::restore_prev(&exe, &prev).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"OLD-BINARY");

        let _ = fs::remove_dir_all(&dir);
    }
}
