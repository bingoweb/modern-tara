//! The run's scratch folder `<temp>/modern-tara-<pid>-<n>`: removed when the
//! guard drops (normal exit, error, panic). A run killed outright cannot clean
//! up, so each run holds an exclusive lock file and later runs remove folders
//! whose lock is free.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

static SEQ: AtomicU32 = AtomicU32::new(0);
const LOCK: &str = ".modern-tara.lock";
const PREFIX: &str = "modern-tara-";

pub struct TempRoot {
    path: PathBuf,
    lock: Option<File>,
}

impl TempRoot {
    pub fn create() -> Result<TempRoot, String> {
        // The long form: git and dotnet print long names even when TEMP holds
        // an 8.3 short name (`RUNNER~1`).
        let base = real_path(&std::env::temp_dir());
        sweep(&base, Duration::from_secs(60));
        for _ in 0..100 {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{PREFIX}{}-{n}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => {
                    let lock = lock_file(&path.join(LOCK)).ok();
                    return Ok(TempRoot { path, lock });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("cannot create {}: {e}", path.display())),
            }
        }
        Err("cannot create a temp folder".into())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A fresh subfolder for one stack or pass.
    pub fn sub(&self, name: &str) -> Result<PathBuf, String> {
        let dir = self.path.join(name);
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        Ok(dir)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        self.lock.take();
        remove_with_retry(&self.path);
    }
}

/// Removes a folder; files a just-killed child still holds get a few retries.
pub fn remove_with_retry(path: &Path) {
    for attempt in 0..5 {
        match fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(Duration::from_millis(100 * (attempt + 1))),
        }
    }
}

/// An advisory exclusive lock (`File::try_lock`), held for the run's life;
/// the OS drops it when the process dies, however it dies.
fn lock_file(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(std::io::ErrorKind::WouldBlock.into()),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Leftovers of killed runs: older than `min_age` and lock free. A folder
/// whose lock is held (a live run) or cannot be opened is left alone.
fn sweep(base: &Path, min_age: Duration) {
    let Ok(entries) = fs::read_dir(base) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_run_folder(&name) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).unwrap_or_default() >= min_age);
        if !old {
            continue;
        }
        let lock = entry.path().join(LOCK);
        if lock.exists() {
            let Ok(probe) = lock_file(&lock) else { continue };
            drop(probe);
        }
        let _ = fs::remove_dir_all(entry.path());
    }
}

/// Only `modern-tara-<pid>-<n>`: never another folder that shares the prefix.
fn is_run_folder(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PREFIX) else { return false };
    let Some((pid, n)) = rest.split_once('-') else { return false };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(pid) && digits(n)
}

/// `p` as the file system names it: 8.3 short names (`RUNNER~1`) expanded,
/// the real letter case, symlinks resolved, and no `\\?\` prefix, so it
/// matches the paths git, dotnet and the other tools print. Unchanged when
/// `p` does not exist.
pub fn real_path(p: &Path) -> PathBuf {
    let Ok(real) = fs::canonicalize(p) else { return p.to_path_buf() };
    if cfg!(windows) {
        let text = real.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\")
            && rest.as_bytes().get(1) == Some(&b':')
        {
            return PathBuf::from(rest);
        }
    }
    real
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GitHub's Windows runners set TEMP to an 8.3 path (`RUNNER~1`) while
    /// git and dotnet print the long one; both must give the same path.
    #[cfg(windows)]
    #[test]
    fn real_path_expands_short_names_and_fixes_case() {
        use std::os::windows::process::CommandExt;
        let long = std::env::temp_dir().join(format!("mt-long-folder-name-{}", std::process::id()));
        fs::create_dir_all(&long).unwrap();
        let real = real_path(&long);
        assert!(!real.to_string_lossy().starts_with(r"\\?\"), "{}", real.display());
        assert!(real.ends_with(long.file_name().unwrap()), "{}", real.display());
        assert_eq!(real_path(&PathBuf::from(long.to_string_lossy().to_uppercase())), real);
        let out = std::process::Command::new("cmd").raw_arg(format!("/d /c for %I in (\"{}\") do @echo %~sI", long.display())).output().unwrap();
        let short = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        if short.is_dir() {
            assert_eq!(real_path(&short), real, "short name {}", short.display());
        }
        let missing = long.join("missing");
        assert_eq!(real_path(&missing), missing);
        let _ = fs::remove_dir_all(&long);
    }

    #[test]
    fn sweep_only_matches_run_folders() {
        assert!(is_run_folder("modern-tara-1234-0"));
        assert!(!is_run_folder("modern-tara-cache"));
        assert!(!is_run_folder("modern-tara-test-rust-12"));
        assert!(!is_run_folder("modern-tara-12-"));
    }

    #[test]
    fn guard_removes_folder() {
        let root = TempRoot::create().unwrap();
        let p = root.path().to_path_buf();
        let sub = root.sub("x/y").unwrap();
        fs::write(sub.join("f.txt"), b"1").unwrap();
        assert!(p.is_dir());
        drop(root);
        assert!(!p.exists());
    }

    #[test]
    fn live_lock_is_held() {
        let root = TempRoot::create().unwrap();
        assert!(lock_file(&root.path().join(LOCK)).is_err());
    }

    #[test]
    fn sweep_removes_dead_runs_only() {
        let base = std::env::temp_dir().join(format!("mt-sweep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        for d in ["modern-tara-1-0", "modern-tara-2-0", "modern-tara-cache"] {
            fs::create_dir_all(base.join(d)).unwrap();
        }
        fs::write(base.join("modern-tara-1-0").join(LOCK), b"").unwrap();
        let held = lock_file(&base.join("modern-tara-2-0").join(LOCK)).unwrap();
        sweep(&base, Duration::ZERO);
        assert!(!base.join("modern-tara-1-0").exists(), "dead run kept");
        assert!(base.join("modern-tara-2-0").exists(), "live run removed");
        assert!(base.join("modern-tara-cache").exists(), "foreign folder removed");
        drop(held);
        let _ = fs::remove_dir_all(&base);
    }
}
