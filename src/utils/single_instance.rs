//! Single-instance locking for the GUI binaries.
//!
//! # The problem with checking whether the file exists
//!
//! The obvious approach is `O_CREAT | O_EXCL` on a path in `/tmp`, plus a
//! `Drop` guard that removes it. That is what this replaces, and it breaks in
//! one common case: `Drop` does not run when a process is killed. Ctrl+C,
//! SIGTERM, `kill -9`, an OOM kill or a crash all leave the file behind.
//!
//! After that, every launch says "already running" about a process that exited
//! hours ago. The only fix is deleting the file by hand, which is not something
//! a graphical installer's user will know to do.
//!
//! # What this does instead
//!
//! It takes an `flock(LOCK_EX | LOCK_NB)` on an open file descriptor. The
//! kernel drops an advisory lock when the descriptor closes, and descriptors
//! close however the process dies. So a file left over from a killed instance
//! is just a file: the next launch opens it, takes the lock, and carries on.
//!
//! The file is still removed on a clean exit, and on SIGINT or SIGTERM through
//! the path registered with [`crate::utils::signal`]. Nothing depends on either
//! happening. After `SIGKILL` or a power cut the file survives and the next
//! launch re-locks it.
//!
//! # One race to know about
//!
//! Using `flock` and also deleting the file has a known hazard. Process A holds
//! the lock. B opens the same path. A exits and deletes the file. B now holds a
//! lock on a file with no name, C creates a fresh file and locks that, and both
//! think they are the only instance.
//!
//! [`InstanceLock::acquire`] avoids this by checking, after taking the lock,
//! that the path still points at the file it locked. If it does not, the file
//! was deleted underneath it, and it tries again with the new one.

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// How many times to re-open after losing the race described above.
///
/// Each retry means another process deleted the file between our open and our
/// lock. That is rare, but a limit stops this spinning forever if instances
/// keep starting and stopping.
const MAX_ATTEMPTS: usize = 5;

/// Why a lock could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another live process holds the lock.
    AlreadyRunning,
    /// The lock file could not be created or opened.
    Io(io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "another instance is already running"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

/// A held single-instance lock. Releasing happens on drop, and — for a process
/// that is killed rather than dropped — when the kernel closes the descriptor.
pub struct InstanceLock {
    path: PathBuf,
    /// Holds both the descriptor and the advisory lock; releasing it releases
    /// the lock, and so does the process dying.
    lock: Option<Flock<File>>,
}

impl InstanceLock {
    /// Take the lock at `path`, or report why not.
    ///
    /// The file is created if missing (mode `0600`), but not created
    /// exclusively. A file left behind by a killed process is expected here,
    /// because the lock is the `flock`, not the file.
    ///
    /// Dropping `O_EXCL` costs a protection that has to be replaced. These
    /// locks live at fixed paths in a world-writable `/tmp`, and the GUIs run
    /// as root through polkit. `O_EXCL` used to make the kernel refuse a path
    /// that was a symlink; without it, a local user could point that name at
    /// any file and have root open it read-write. So open with `O_NOFOLLOW`
    /// and refuse anything that is not a regular file.
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, LockError> {
        let path = path.as_ref().to_path_buf();

        for _ in 0..MAX_ATTEMPTS {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .map_err(LockError::Io)?;

            // O_NOFOLLOW stops a symlink, but not a FIFO, socket or device
            // planted at the same name. None of those is a lock file.
            let meta = file.metadata().map_err(LockError::Io)?;
            if !meta.file_type().is_file() {
                return Err(LockError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a regular file", path.display()),
                )));
            }

            // Non-blocking, so a second instance is told straight away rather
            // than waiting for the first to exit. Only EWOULDBLOCK means
            // someone else holds the lock; any other error is a real failure
            // and should not be reported as "already running".
            let lock = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
                Ok(l) => l,
                Err((_, Errno::EWOULDBLOCK)) => return Err(LockError::AlreadyRunning),
                Err((_, errno)) => {
                    return Err(LockError::Io(io::Error::from_raw_os_error(errno as i32)))
                }
            };

            // We have a lock, but on which file? If another instance deleted
            // this path between our open and our flock, we are holding a file
            // with no name, and the next process will lock its replacement.
            // Check that the path still points at what we locked.
            let locked_ino = meta.ino();
            match std::fs::metadata(&path) {
                Ok(m) if m.ino() == locked_ino => {
                    // A killed process runs no destructor, so for SIGINT and
                    // SIGTERM the signal handler is the only thing that can
                    // remove the file. The binary installs the handlers, not
                    // this function; registering with none installed does
                    // nothing.
                    crate::utils::signal::register_cleanup_path(&path);
                    return Ok(Self {
                        path,
                        lock: Some(lock),
                    });
                }
                // Deleted or replaced underneath us. Drop this descriptor,
                // which releases the useless lock, and try the current file.
                _ => continue,
            }
        }

        Err(LockError::Io(io::Error::new(
            io::ErrorKind::WouldBlock,
            "lock file kept being replaced while acquiring it",
        )))
    }

    /// The path being held.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Deregister first. Once this lock is released another process may
        // take the path, and a later signal must not delete their file.
        crate::utils::signal::unregister_cleanup_path();
        // Delete before closing. The descriptor still holds the lock while the
        // name goes away, so a process that opened the old path cannot both
        // take the lock and pass the path check. It retries and gets the new
        // file.
        let _ = std::fs::remove_file(&self.path);
        drop(self.lock.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "deploytix-instance-test-{}-{}.lock",
            tag,
            std::process::id()
        ))
    }

    #[test]
    fn a_lock_is_taken_and_released() {
        let p = tmp_path("basic");
        let _ = std::fs::remove_file(&p);

        let lock = InstanceLock::acquire(&p).expect("first acquire succeeds");
        assert!(p.exists(), "the lock file is created");
        drop(lock);
        assert!(!p.exists(), "and removed again on clean exit");
    }

    #[test]
    fn a_second_instance_is_refused_while_the_first_holds_it() {
        let p = tmp_path("contended");
        let _ = std::fs::remove_file(&p);

        let first = InstanceLock::acquire(&p).expect("first acquire succeeds");
        match InstanceLock::acquire(&p) {
            Err(LockError::AlreadyRunning) => {}
            Err(e) => panic!("expected AlreadyRunning, got {e}"),
            Ok(_) => panic!("a second instance must not acquire the lock"),
        }
        drop(first);

        // ...and the next one gets in once the holder is gone.
        InstanceLock::acquire(&p).expect("acquire succeeds after release");
        let _ = std::fs::remove_file(&p);
    }

    /// The bug this module exists to fix. A killed process leaves the file
    /// behind but not the lock, and the old existence check turned that into a
    /// permanent refusal to start.
    #[test]
    fn a_leftover_file_from_a_killed_process_does_not_block_startup() {
        let p = tmp_path("stale");
        // Exactly what a SIGKILLed instance leaves: the file, no holder.
        std::fs::write(&p, b"").unwrap();
        assert!(p.exists());

        let lock = InstanceLock::acquire(&p)
            .expect("a stale file must not be mistaken for a running instance");
        drop(lock);
        assert!(!p.exists());
    }

    /// Closing the descriptor releases the lock, so a lock whose owner
    /// disappeared without deleting the file is still available to the next
    /// process. This simulates that.
    #[test]
    fn releasing_the_descriptor_releases_the_lock() {
        let p = tmp_path("fd");
        let _ = std::fs::remove_file(&p);

        {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&p)
                .unwrap();
            let _held = Flock::lock(f, FlockArg::LockExclusiveNonblock)
                .map_err(|(_, e)| e)
                .unwrap();
            // _held drops here: the lock is released, the file stays.
        }

        assert!(p.exists(), "the file outlives the lock");
        InstanceLock::acquire(&p).expect("the released lock is available again");
        let _ = std::fs::remove_file(&p);
    }

    /// These locks sit at fixed paths in a world-writable `/tmp` and the GUIs
    /// run as root, so a symlink planted at the name must not be followed.
    /// `O_EXCL` used to give this for free; `O_NOFOLLOW` replaces it.
    #[test]
    fn a_symlink_at_the_lock_path_is_refused() {
        let p = tmp_path("symlink");
        let target = tmp_path("symlink-target");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&target);

        std::os::unix::fs::symlink(&target, &p).unwrap();
        match InstanceLock::acquire(&p) {
            Err(LockError::Io(_)) => {}
            Err(e) => panic!("expected an I/O error, got {e}"),
            Ok(_) => panic!("a symlinked lock path must be refused"),
        }
        assert!(!target.exists(), "and the target must not be created");

        let _ = std::fs::remove_file(&p);
    }

    /// O_NOFOLLOW does not stop a FIFO planted at the same name.
    #[test]
    fn a_non_regular_file_at_the_lock_path_is_refused() {
        let p = tmp_path("fifo");
        let _ = std::fs::remove_file(&p);
        let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);

        match InstanceLock::acquire(&p) {
            Err(LockError::Io(_)) => {}
            Err(e) => panic!("expected an I/O error, got {e}"),
            Ok(_) => panic!("a FIFO must not be accepted as a lock file"),
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn the_lock_reports_its_path() {
        let p = tmp_path("path");
        let _ = std::fs::remove_file(&p);
        let lock = InstanceLock::acquire(&p).unwrap();
        assert_eq!(lock.path(), p.as_path());
    }
}
