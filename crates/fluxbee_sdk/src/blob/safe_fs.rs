//! Writes into the blob tree without acting through a name someone else controls (FINDINGS A-56).
//!
//! The processes that write blobs run as root, but `active/` and `public/` belong to the Syncthing
//! user, who can rename or plant names inside them. So root never follows a name there: a
//! directory is opened without following a link at its last component, files are created
//! exclusively and moved by descriptor, and a blob takes the owner of the folder it lands in, so
//! the Syncthing that serves that folder can read it.

use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

use nix::errno::Errno;
use nix::fcntl::{openat, OFlag};
use nix::sys::stat::{fchmod, fstat, mkdirat, Mode, SFlag};

/// Opens `name` as a directory, relative to `parent` when given, never through a symlink at its
/// last component.
pub fn open_dir_no_follow(parent: Option<&OwnedFd>, name: &Path) -> io::Result<OwnedFd> {
    let fd = openat(
        parent.map(|dir| dir.as_raw_fd()),
        name,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    // SAFETY: openat just returned this descriptor and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Opens `name` under `parent` to read, never through a symlink, and without blocking on a FIFO
/// planted under that name (the caller checks what it opened, e.g. with `is_regular_file`).
pub fn open_file_no_follow(parent: &OwnedFd, name: &str) -> io::Result<OwnedFd> {
    let fd = openat(
        Some(parent.as_raw_fd()),
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    // SAFETY: as above.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Creates `name` under `parent` exclusively (it must not exist, not even as a link), 0640.
pub fn create_file_exclusive(parent: &OwnedFd, name: &str) -> io::Result<std::fs::File> {
    let fd = openat(
        Some(parent.as_raw_fd()),
        name,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o640),
    )?;
    // SAFETY: as above.
    Ok(std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// `name` under `parent`, created 0750 when missing, opened without following a link, and given
/// to `owner` when one is named and it has another. An existing directory keeps its mode: its
/// owner (the orchestrator, for `active/`) sets it.
pub fn ensure_child_dir(
    parent: &OwnedFd,
    name: &str,
    owner: Option<(u32, u32)>,
) -> io::Result<OwnedFd> {
    let created = match mkdirat(
        Some(parent.as_raw_fd()),
        name,
        Mode::from_bits_truncate(0o750),
    ) {
        Ok(()) => true,
        Err(Errno::EEXIST) => false,
        Err(err) => return Err(err.into()),
    };
    let dir = open_dir_no_follow(Some(parent), Path::new(name))?;
    if let Some(owner) = owner {
        give_to(&dir, owner)?;
    }
    if created {
        // mkdirat's mode went through the umask.
        fchmod(dir.as_raw_fd(), Mode::from_bits_truncate(0o750))?;
    }
    Ok(dir)
}

/// Whether an open file is a regular file.
pub fn is_regular_file(fd: &impl AsFd) -> io::Result<bool> {
    let stat = fstat(fd.as_fd().as_raw_fd())?;
    Ok(SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFREG)
}

/// Whether root may give away a staged blob it holds open: a regular file that is either one this
/// process wrote (one link, its own), one a concurrent put of the same blob already replaced (no
/// link left), or one a concurrent promote already handed to `owner`. Nothing anyone else can
/// reach another way.
pub fn is_handable_staged_file(fd: &impl AsFd, owner: (u32, u32)) -> io::Result<bool> {
    let stat = fstat(fd.as_fd().as_raw_fd())?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { nix::libc::geteuid() };
    let regular = SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFREG;
    Ok(regular
        && (stat.st_nlink == 0
            || (stat.st_nlink == 1
                && (stat.st_uid == euid || (stat.st_uid, stat.st_gid) == owner))))
}

/// The (device, inode) of an open file.
pub fn identity_of(fd: &impl AsFd) -> io::Result<(u64, u64)> {
    let stat = fstat(fd.as_fd().as_raw_fd())?;
    #[allow(clippy::unnecessary_cast)]
    Ok((stat.st_dev as u64, stat.st_ino as u64))
}

/// The owner (uid, gid) of an open file or directory.
pub fn owner_of(fd: &impl AsFd) -> io::Result<(u32, u32)> {
    let stat = fstat(fd.as_fd().as_raw_fd())?;
    Ok((stat.st_uid, stat.st_gid))
}

/// Gives `fd` to `owner` when it has another one. Nothing to do, and no privilege needed, when it
/// already has that owner; otherwise only root can, and failing is an error: a blob the Syncthing
/// user cannot read would just never sync.
pub fn give_to(fd: &impl AsFd, owner: (u32, u32)) -> io::Result<()> {
    if owner_of(fd)? != owner {
        std::os::unix::fs::fchown(fd, Some(owner.0), Some(owner.1))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fluxbee-safe-fs-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_symlinked_directory_is_never_opened() {
        let base = temp_dir("dir-link");
        let target = base.join("target");
        std::fs::create_dir_all(&target).unwrap();
        symlink(&target, base.join("link")).unwrap();
        // Linux answers ELOOP, macOS ENOTDIR: refused either way, never followed.
        let err = open_dir_no_follow(None, &base.join("link")).unwrap_err();
        assert!(
            [Some(nix::libc::ELOOP), Some(nix::libc::ENOTDIR)].contains(&err.raw_os_error()),
            "{err}"
        );
        let parent = open_dir_no_follow(None, &base).unwrap();
        // ensure_child_dir over a planted link refuses instead of following it.
        assert!(ensure_child_dir(&parent, "link", None).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_exclusive_create_refuses_a_planted_name() {
        let base = temp_dir("excl");
        let parent = open_dir_no_follow(None, &base).unwrap();
        symlink("/nonexistent-target", base.join("planted")).unwrap();
        assert!(create_file_exclusive(&parent, "planted").is_err());
        assert!(!Path::new("/nonexistent-target").exists());
        let mut file = create_file_exclusive(&parent, "fresh").unwrap();
        file.write_all(b"x").unwrap();
        let mode = std::fs::metadata(base.join("fresh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode & 0o027,
            0,
            "never group-writable or world-readable: {mode:o}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_child_directory_is_made_0750_and_keeps_its_owner_when_it_is_ours() {
        let base = temp_dir("child");
        let parent = open_dir_no_follow(None, &base).unwrap();
        let own = owner_of(&parent).unwrap();
        let child = ensure_child_dir(&parent, "ab", Some(own)).unwrap();
        assert_eq!(owner_of(&child).unwrap(), own);
        let mode = std::fs::metadata(base.join("ab"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o750);
        // Existing: opened again, not an error.
        ensure_child_dir(&parent, "ab", Some(own)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }
}
