//! One process of a core binary per host: an flock on a file in `/var/run/fluxbee`, which the
//! kernel releases when the process ends, however it ends (FINDINGS A-180). A binary systemd starts
//! takes it before it acts on the host, so a second copy started by hand (`--version`, say) exits
//! before it touches anything. The lock is on tmpfs, as SY.vault's and SY.opa.rules' are: a full or
//! read-only disk never keeps a node down. rt-gateway holds its own (`hold_instance_lock` in
//! `src/router/mod.rs`, FINDINGS A-177), which moves here at the router's next release.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::Mutex;

/// This process's locks (`hold_for_process`), each open until the process ends.
static HELD: Mutex<Vec<fs::File>> = Mutex::new(Vec::new());

/// Makes this process the only `what` on the host until it ends: `hold`, kept for the process's
/// whole life. `Err` names the holder.
pub fn hold_for_process(path: &Path, what: &str) -> Result<(), String> {
    if let Some(file) = hold(path, what)? {
        HELD.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(file);
    }
    Ok(())
}

/// An flock on `path`. `Err` only when another process holds it, naming its PID. A lock that cannot
/// be opened or taken for any other reason is warned and `Ok(None)`: no sign of another copy, so the
/// caller runs without one. The file is never removed, as a removal would let a third process lock a
/// new file beside one still held; it holds the holder's PID, for whoever looks.
pub fn hold(path: &Path, what: &str) -> Result<Option<fs::File>, String> {
    let opened = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| {
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
        });
    let mut file = match opened {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(
                lock = %path.display(),
                error = %err,
                "{what} could not open its instance lock; it runs without one"
            );
            return Ok(None);
        }
    };
    // SAFETY: the descriptor is open and owned by `file` for the whole call.
    if unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::WouldBlock {
            tracing::warn!(
                lock = %path.display(),
                error = %err,
                "{what} could not take its instance lock; it runs without one"
            );
            return Ok(None);
        }
        let holder = fs::read_to_string(path).unwrap_or_default();
        return Err(format!(
            "another {what} runs here (PID {}, it holds {}); this one exits and touches nothing",
            holder.trim(),
            path.display()
        ));
    }
    if let Err(err) = file
        .set_len(0)
        .and_then(|()| writeln!(file, "{}", std::process::id()))
    {
        tracing::warn!(
            lock = %path.display(),
            error = %err,
            "{what} holds its instance lock and could not write its PID in it"
        );
    }
    Ok(Some(file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn test_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "fbinstance-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ))
    }

    /// A second copy is refused while the first holds the lock, and named its PID; once the first
    /// ends, the lock is free again. The file stays where it is.
    #[test]
    fn a_second_copy_is_refused_while_the_first_holds_the_lock() {
        let dir = test_dir();
        let path = dir.join("sy-orchestrator.lock");
        let first = hold(&path, "sy-orchestrator")
            .expect("the first")
            .expect("its lock");
        let mode = fs::metadata(&path).expect("lock file").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let pid = std::process::id().to_string();
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), pid);

        let refused = hold(&path, "sy-orchestrator").expect_err("a second copy");
        assert!(
            refused.contains("another sy-orchestrator runs here"),
            "{refused}"
        );
        assert!(refused.contains(&format!("PID {pid}")), "{refused}");
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), pid);

        drop(first);
        assert!(hold(&path, "sy-orchestrator").expect("free").is_some());
        assert!(path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A lock that cannot be opened is no sign of another copy: the caller runs without one.
    #[test]
    fn a_lock_that_cannot_be_opened_never_keeps_a_node_down() {
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("dir");
        let not_a_dir = dir.join("a-file");
        fs::write(&not_a_dir, b"").expect("a file where the lock's folder would be");
        let held = hold(&not_a_dir.join("x.lock"), "sy-orchestrator");
        assert!(matches!(held, Ok(None)), "{held:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}
