//! File helpers shared by the secret stores and the config file.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Creates `dir` and its parents; the directory gets mode 0700 on Unix.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes `data` to a temporary file next to `path`, syncs it and renames it
/// into place, so a crash never leaves a half-written file under the final
/// name. With `private`, the file gets mode 0600 on Unix.
pub fn write_atomic(path: &Path, data: &[u8], private: bool) -> io::Result<()> {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp{}.{}", std::process::id(), seq));
    let _ = fs::remove_file(&tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(if private { 0o600 } else { 0o644 });
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut file = opts.open(&tmp)?;
    let written = file.write_all(data).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Tightens even an already existing directory to 0700.
        create_private_dir(&sub).unwrap();
        let path = sub.join("file.toml");
        write_atomic(&path, b"one", false).unwrap();
        write_atomic(&path, b"two", true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_dir(&sub).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&sub), 0o700);
            assert_eq!(mode(&path), 0o600);
        }
    }
}
