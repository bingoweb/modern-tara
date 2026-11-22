//! One module per stack. Each turns its changed files into a `StackReport`
//! with findings; the caller filters them to changed lines.

pub mod astgrep;
pub mod csharp;
pub mod go;
pub mod js;
pub mod powershell;
pub mod python;
pub mod rust;

use crate::change::Change;
use crate::copy::TempRoot;
use crate::proc::{self, Cmd};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const ALL: [&str; 7] = ["csharp", "rust", "python", "js", "css", "powershell", "go"];

pub fn extensions(stack: &str) -> &'static [&'static str] {
    match stack {
        "csharp" => &["cs"],
        "rust" => &["rs"],
        "python" => &["py", "pyi"],
        "js" => &["js", "mjs", "cjs", "ts", "mts", "cts", "tsx", "jsx"],
        "css" => &["css"],
        "powershell" => &["ps1", "psm1"],
        "go" => &["go"],
        _ => &[],
    }
}

/// Executables, resolved once; `None` = not installed.
#[derive(Default, Clone)]
pub struct Tools {
    pub cargo: Option<PathBuf>,
    pub rustc: Option<PathBuf>,
    pub dotnet: Option<PathBuf>,
    pub ruff: Option<PathBuf>,
    pub node: Option<PathBuf>,
    pub pwsh: Option<PathBuf>,
    pub ast_grep: Option<PathBuf>,
    pub go: Option<PathBuf>,
}

impl Tools {
    pub fn find() -> Tools {
        let home = std::env::home_dir().unwrap_or_default();
        let cargo_bin = std::env::var_os("CARGO_HOME").map_or_else(|| home.join(".cargo"), PathBuf::from).join("bin");
        // Where scripts/install-tools.ps1 and scripts/install-tools.sh put ruff and ast-grep.
        let local_bin = home.join(".local").join("bin");
        Tools {
            cargo: proc::which("cargo", &[exe_in(&cargo_bin, "cargo")]),
            rustc: proc::which("rustc", &[exe_in(&cargo_bin, "rustc")]),
            dotnet: proc::which("dotnet", &fallbacks("dotnet")),
            ruff: proc::which("ruff", &[exe_in(&local_bin, "ruff")]),
            node: proc::which("node", &fallbacks("node")),
            pwsh: proc::which("pwsh", &fallbacks("pwsh")),
            ast_grep: proc::which("ast-grep", &[exe_in(&local_bin, "ast-grep"), exe_in(&cargo_bin, "ast-grep")]),
            go: proc::which("go", &fallbacks("go")),
        }
    }
}

/// `dir/name` with the platform's executable suffix.
pub fn exe_in(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Usual install locations of `name`, tried when it is not on PATH.
pub fn fallbacks(name: &str) -> Vec<PathBuf> {
    if cfg!(windows) {
        let pf = std::env::var_os("ProgramFiles").map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from);
        match name {
            "git" => vec![pf.join("Git").join("cmd").join("git.exe")],
            "dotnet" => vec![pf.join("dotnet").join("dotnet.exe")],
            "node" => vec![pf.join("nodejs").join("node.exe")],
            "pwsh" => vec![pf.join("PowerShell").join("7").join("pwsh.exe")],
            "go" => vec![pf.join("Go").join("bin").join("go.exe")],
            _ => Vec::new(),
        }
    } else {
        let home = std::env::home_dir().unwrap_or_default();
        match name {
            "dotnet" => vec![home.join(".dotnet").join("dotnet"), PathBuf::from("/usr/share/dotnet/dotnet"), PathBuf::from("/usr/lib/dotnet/dotnet")],
            "pwsh" => vec![PathBuf::from("/opt/microsoft/powershell/7/pwsh"), PathBuf::from("/usr/bin/pwsh")],
            "go" => vec![PathBuf::from("/usr/local/go/bin/go"), PathBuf::from("/usr/lib/go/bin/go")],
            "git" | "node" => vec![PathBuf::from("/usr/bin").join(name)],
            _ => Vec::new(),
        }
    }
}

pub struct Ctx<'a> {
    pub change: &'a Change,
    pub temp: &'a TempRoot,
    /// The modern-tara folder: `js/`, `rules/`, `ps/`, `sgconfig.yml`.
    pub home: PathBuf,
    pub tools: Tools,
    /// `--timeout`, else each stack's default.
    pub timeout: Option<Duration>,
}

impl Ctx<'_> {
    pub fn timeout_or(&self, default_secs: u64) -> Duration {
        self.timeout.unwrap_or(Duration::from_secs(default_secs))
    }
}

/// First line of `<exe> --version`, trimmed.
pub fn version_line(exe: &Path, args: &[&str]) -> Option<String> {
    // A neutral cwd: a rust-toolchain.toml or global.json in the caller's
    // folder must not change (or install) the tool being probed.
    let out = Cmd::new(exe).args(args).cwd(&std::env::temp_dir()).timeout(Duration::from_secs(30)).run().ok()?;
    out.ok().then(|| out.stdout_text().lines().next().unwrap_or("").trim().to_string()).filter(|s| !s.is_empty())
}

/// Repo-relative form of a path a tool printed for a file in `copy`.
pub fn rel_to(copy: &Path, printed: &str) -> Option<String> {
    let printed = printed.replace('\\', "/");
    if let Some(rest) = crate::change::strip_dir(&printed, &copy.to_string_lossy()) {
        return crate::change::normalize(&rest);
    }
    if Path::new(&printed).is_absolute() || printed.contains(':') {
        return None;
    }
    crate::change::normalize(printed.trim_start_matches("./"))
}

/// FNV-1a, for stable cache folder names.
pub fn fnv64(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The OS cache folder: `%LOCALAPPDATA%` on Windows, `~/Library/Caches` on
/// macOS, else `$XDG_CACHE_HOME` or `~/.cache`.
pub fn os_cache_dir() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute());
    if cfg!(windows) {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        std::env::home_dir().map(|h| h.join("Library").join("Caches"))
    } else {
        var("XDG_CACHE_HOME").or_else(|| std::env::home_dir().map(|h| h.join(".cache")))
    }
}

/// `<OS cache folder>/modern-tara/cache/<hash>` for this repo and key;
/// `MODERN_TARA_CACHE` replaces `<OS cache folder>/modern-tara/cache`.
pub fn cache_dir(repo: &Path, key: &str) -> PathBuf {
    let base = std::env::var_os("MODERN_TARA_CACHE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| os_cache_dir().map(|d| d.join("modern-tara").join("cache")))
        .unwrap_or_else(|| std::env::temp_dir().join("modern-tara-cache"));
    let path = repo.to_string_lossy();
    // Windows paths ignore case and take either slash; elsewhere they are exact.
    let path = if cfg!(windows) { path.to_lowercase().replace('/', "\\") } else { path.into_owned() };
    base.join(format!("{:016x}", fnv64(&format!("{path}|{key}"))))
}

/// Nearest file named `name` (case-insensitive) in `dir` or above, from the listing.
pub fn nearest(change: &Change, dir: &str, name: &str) -> Option<String> {
    let listing = change.listing().ok()?;
    crate::change::ancestors(dir).into_iter().find_map(|d| {
        let candidate = crate::change::join(&d, name);
        if let Ok(i) = listing.binary_search(&candidate) {
            return Some(listing[i].clone());
        }
        let prefix = if d.is_empty() { String::new() } else { format!("{d}/") };
        listing.iter().find(|p| p.len() == candidate.len() && p.starts_with(&prefix) && p.eq_ignore_ascii_case(&candidate)).cloned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_to_strips_copy_prefix() {
        let copy = Path::new(r"C:\Temp\modern-tara-1-0\rust");
        assert_eq!(rel_to(copy, r"C:\Temp\modern-tara-1-0\rust\src\main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(rel_to(copy, r"c:/temp/MODERN-TARA-1-0/rust/a/b.rs").as_deref(), Some("a/b.rs"));
        assert_eq!(rel_to(copy, "src\\lib.rs").as_deref(), Some("src/lib.rs"));
        assert_eq!(rel_to(copy, r"C:\Users\x\.cargo\registry\a.rs"), None);
        // A byte cut at the copy path's length would land inside `ş`.
        let short = Path::new("C:/T/abc");
        assert_eq!(rel_to(short, "C:/T/aaş.rs"), None);
        assert_eq!(rel_to(Path::new("C:/T/mt-ş"), "C:/t/MT-ş/src/aşşşşşşş.rs").as_deref(), Some("src/aşşşşşşş.rs"));
    }

    #[test]
    fn cache_dir_is_stable_and_keyed() {
        let a = cache_dir(Path::new(r"C:\Repo"), "rust|");
        let b = cache_dir(Path::new("c:/repo"), "rust|");
        let c = cache_dir(Path::new(r"C:\Repo"), "rust|sub");
        if cfg!(windows) {
            assert_eq!(a, b);
        }
        assert_ne!(a, c);
    }
}
