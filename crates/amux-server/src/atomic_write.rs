//! Replace a file's contents so a reader never sees it half-written and no
//! name planted in its directory can redirect the write. One helper for every
//! "write then rename" site (PR #48 review B1; ai-config #211).
//!
//! The copies this replaces each staged into a PREDICTABLE temp name
//! (`.{name}.amux-save-{pid}-{seq}`, `{name}.tmp-{pid}`, ...) opened with
//! `std::fs::write`, which follows symlinks and has no O_EXCL: anyone who could
//! create entries in the directory planted that name as a link and the save
//! wrote through it (a reviewer overwrote `~/.bashrc` that way). Here the temp
//! file is created by `tempfile` with a random name and O_EXCL, and the mode
//! is set on the open handle, never by path.
//!
//! Kept: the bytes, atomically (temp written, synced, renamed over the target,
//! directory synced); the existing file's permission bits; every name of a
//! hard-linked file (written in place instead, logged). Refused: a regular
//! file this process cannot write (a read-only file stays read-only, as an
//! in-place write would have left it).
//!
//! NOT kept, by design and said here rather than discovered: the owner and
//! group (the new file belongs to the writer), extended attributes (Finder
//! tags, `com.apple.*`) and ACLs. That is acceptable for the files amux writes
//! and edits; a caller that needs them must not use this helper.

use std::io::Write;
use std::path::Path;

/// How a write landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landed {
    /// A complete new file was renamed over the target.
    Replaced,
    /// The target has more than one hard link, so it was rewritten in place to
    /// keep every name pointing at the same contents.
    InPlaceHardLinked,
}

/// Replace `target` with `bytes`.
///
/// `target` itself is replaced, not followed: when it is a symlink, the link
/// is replaced by a regular file. A caller that saves THROUGH a link resolves
/// it first, with the same resolution its write gates judged (`file_viewer`
/// passes `resolve_nonstrict(path)`). The parent directory must exist.
pub fn atomic_write(target: &Path, bytes: &[u8]) -> std::io::Result<Landed> {
    #[cfg(test)]
    if failing::matches(target) {
        return Err(std::io::Error::other(format!("injected write failure: {}", target.display())));
    }
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let existing = std::fs::symlink_metadata(target).ok();
    let mut keep_mode = None;
    if let Some(meta) = existing.as_ref().filter(|m| m.file_type().is_file()) {
        // Opening for write (no truncate, no follow) is the permission check:
        // a file this process may not write is refused here, untouched.
        let file = open_existing_for_write(target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.nlink() > 1 {
                file.set_len(0)?;
                (&file).write_all(bytes)?;
                file.sync_all()?;
                tracing::info!(target: "amux::file", path = %target.display(), links = meta.nlink(),
                    measured = true, n_considered = 1, verdict = "atomic_write_in_place_hardlinked",
                    "file has several hard links: rewritten in place so every name keeps the new contents");
                return Ok(Landed::InPlaceHardLinked);
            }
        }
        drop(file);
        keep_mode = Some(meta.permissions());
    }
    let mut builder = tempfile::Builder::new();
    builder.prefix(".amux-save-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // A new file gets what std::fs::write would have given it (0666 less
        // the umask), not tempfile's private 0600.
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut tmp = builder.tempfile_in(dir)?;
    tmp.write_all(bytes)?;
    if let Some(mode) = keep_mode {
        tmp.as_file().set_permissions(mode)?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(target).map_err(|e| e.error)?;
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        // The rename is durable once the directory entry is.
        let _ = d.sync_all();
    }
    Ok(Landed::Replaced)
}

fn open_existing_for_write(target: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(target)
}

/// Test support: make writes to one path fail on this thread while the guard
/// lives, for tests of a caller's recovery path. (The old copies were failed by
/// planting a directory at their predictable temp name; there is none now.)
#[cfg(test)]
pub(crate) mod failing {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    thread_local! { static PATHS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) }; }

    pub(crate) struct Guard(PathBuf);

    impl Drop for Guard {
        fn drop(&mut self) {
            PATHS.with(|p| p.borrow_mut().retain(|x| x != &self.0));
        }
    }

    pub(crate) fn writes_to(path: &Path) -> Guard {
        PATHS.with(|p| p.borrow_mut().push(path.to_path_buf()));
        Guard(path.to_path_buf())
    }

    pub(super) fn matches(target: &Path) -> bool {
        PATHS.with(|p| p.borrow().iter().any(|x| x == target))
    }
}

/// Test support: plant symlinks named `names` in `dir`, all pointing at
/// `victim`, the way an attacker who can create entries in a save's directory
/// would plant the temp name an old write path was about to use.
#[cfg(all(test, unix))]
pub(crate) fn plant_links(dir: &Path, names: &[String], victim: &Path) {
    for n in names {
        let _ = std::os::unix::fs::symlink(victim, dir.join(n));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_planted_temp_name_symlink_cannot_redirect_the_write() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let target = dir.path().join("notes.md");
        std::fs::write(&target, "old").unwrap();
        // Every predictable name the old copies used, planted as links.
        for seq in 0..64 {
            for name in [format!(".notes.md.amux-save-{}-{seq}", std::process::id()),
                         format!(".notes.md.amux-save-{}", std::process::id()),
                         "notes.md.tmp".to_string(), ".notes.md.tmp".to_string(),
                         format!("notes.tmp-{}", std::process::id()),
                         format!("notes.md.{}.tmp", std::process::id())] {
                let _ = std::os::unix::fs::symlink(&victim, dir.path().join(name));
            }
        }
        assert_eq!(atomic_write(&target, b"new").unwrap(), Landed::Replaced);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert!(!std::fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_is_refused_and_kept() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 { return; } // root writes anything
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ro.txt");
        std::fs::write(&target, "protected").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
        let err = atomic_write(&target, b"overwritten").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "protected");
        assert!(std::fs::read_dir(dir.path()).unwrap().count() == 1, "no temp left behind");
    }

    #[cfg(unix)]
    #[test]
    fn a_hard_linked_file_is_rewritten_in_place_so_every_name_agrees() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "v1").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        assert_eq!(atomic_write(&a, b"v2").unwrap(), Landed::InPlaceHardLinked);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "v2");
    }

    #[cfg(unix)]
    #[test]
    fn mode_is_kept_and_a_new_file_gets_the_umask_default() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run.sh");
        std::fs::write(&script, "#!/bin/sh").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        atomic_write(&script, b"#!/bin/sh\necho hi").unwrap();
        assert_eq!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o777, 0o755);
        let fresh = dir.path().join("fresh.txt");
        atomic_write(&fresh, b"x").unwrap();
        let reference = dir.path().join("reference.txt");
        std::fs::write(&reference, "x").unwrap();
        assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
                   std::fs::metadata(&reference).unwrap().permissions().mode() & 0o777,
                   "a new file gets what std::fs::write would have given it");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_target_is_replaced_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let target = dir.path().join("upload.bin");
        std::os::unix::fs::symlink(&victim, &target).unwrap();
        atomic_write(&target, b"data").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(std::fs::read(&target).unwrap(), b"data");
    }
}
