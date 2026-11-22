//! What changed and where file contents come from: a git revision, the index,
//! the work tree, or a plain folder. Stacks only see this through `Change`.

use crate::git::Git;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// Never copied or listed: dependencies, VCS data, caches. Build output
/// (`target/`, `bin/`, `obj/`) is skipped only next to its manifest and when
/// git does not track it, so `src/bin/*.rs` stays a source.
pub const SKIP_DIRS: [&str; 12] =
    [".git", "node_modules", ".venv", "venv", "__pycache__", ".ruff_cache", ".mypy_cache", ".pytest_cache", ".tox", ".vs", ".next", ".turbo"];

/// Larger files are neither copied nor scanned.
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

pub enum Source {
    /// `git show <rev>:<path>`, through one cat-file batch.
    Rev(Git, String),
    /// The index (`--staged`).
    Index(Git),
    /// Files on disk, listed by git (tracked plus untracked, not ignored).
    WorkTree(Git),
    /// Files on disk in a folder that is not a git repository.
    Dir,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lines {
    All,
    /// Inclusive, 1-based, sorted.
    Ranges(Vec<(u32, u32)>),
}

impl Lines {
    pub fn contains(&self, line: u32) -> bool {
        match self {
            Lines::All => true,
            Lines::Ranges(ranges) => ranges.iter().any(|&(a, b)| line >= a && line <= b),
        }
    }
    pub fn count(&self) -> Option<u64> {
        match self {
            Lines::All => None,
            Lines::Ranges(r) => Some(r.iter().map(|&(a, b)| u64::from(b.saturating_sub(a)) + 1).sum()),
        }
    }
}

pub struct Change {
    pub root: PathBuf,
    pub source: Source,
    pub files: BTreeMap<String, Lines>,
    /// The `change` object of the JSON report.
    pub describe: serde_json::Value,
    listing: OnceLock<Result<Vec<String>, String>>,
}

impl Change {
    pub fn new(root: PathBuf, source: Source, files: BTreeMap<String, Lines>, describe: serde_json::Value) -> Change {
        Change { root, source, files, describe, listing: OnceLock::new() }
    }

    /// Every file the source holds (repo-relative, `/`), skip dirs left out.
    pub fn listing(&self) -> Result<&[String], String> {
        let result = self.listing.get_or_init(|| {
            let mut all = match &self.source {
                Source::Rev(g, rev) => g.ls_tree(rev)?,
                Source::Index(g) => g.ls_index()?,
                Source::WorkTree(g) => {
                    let mut v = g.ls_index()?;
                    v.extend(g.ls_untracked()?.into_iter().filter(|p| !is_build_output(&self.root, p)));
                    v.retain(|p| self.root.join(p).is_file());
                    v
                }
                Source::Dir => walk(&self.root),
            };
            all.retain(|p| !in_skip_dir(p));
            all.sort();
            all.dedup();
            Ok(all)
        });
        result.as_deref().map_err(Clone::clone)
    }

    /// Contents of `paths`; unreadable or oversized files are left out.
    pub fn read(&self, paths: &[String]) -> Result<HashMap<String, Vec<u8>>, String> {
        let mut found = match &self.source {
            Source::Rev(g, rev) => g.read_blobs(&format!("{rev}:"), paths, MAX_FILE_BYTES)?,
            Source::Index(g) => g.read_blobs(":", paths, MAX_FILE_BYTES)?,
            Source::WorkTree(_) | Source::Dir => paths
                .iter()
                .filter_map(|p| {
                    let full = safe_join(&self.root, p)?;
                    let meta = fs::metadata(&full).ok()?;
                    (meta.is_file() && usize::try_from(meta.len()).is_ok_and(|n| n <= MAX_FILE_BYTES))
                        .then(|| fs::read(&full).ok().map(|b| (p.clone(), b)))
                        .flatten()
                })
                .collect(),
        };
        found.retain(|_, v| v.len() <= MAX_FILE_BYTES);
        Ok(found)
    }

    pub fn read_text(&self, path: &str) -> Option<String> {
        let mut m = self.read(&[path.to_string()]).ok()?;
        m.remove(path).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    pub fn exists(&self, path: &str) -> bool {
        self.listing().is_ok_and(|l| l.binary_search_by(|p| p.as_str().cmp(path)).is_ok())
    }

    /// Writes `paths` under `dest` keeping their relative layout. Returns how
    /// many were written.
    pub fn materialize(&self, dest: &Path, paths: &[String]) -> Result<usize, String> {
        let contents = self.read(paths)?;
        let mut written = 0;
        for (path, bytes) in &contents {
            let Some(target) = safe_join(dest, path) else { continue };
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            if fs::write(&target, bytes).is_ok() {
                written += 1;
            }
        }
        Ok(written)
    }

    /// Changed files of one stack, by extension (lowercase, no dot).
    pub fn files_with(&self, extensions: &[&str]) -> Vec<String> {
        self.files.keys().filter(|p| extensions.contains(&extension(p).as_str())).cloned().collect()
    }

    /// Listing entries under `dir` ("" = root) passing `keep`.
    pub fn under(&self, dir: &str, keep: impl Fn(&str) -> bool) -> Vec<String> {
        let prefix = if dir.is_empty() { String::new() } else { format!("{}/", dir.trim_end_matches('/')) };
        self.listing().map_or_default(|l| l.iter().filter(|p| p.starts_with(&prefix) && keep(p)).cloned().collect())
    }
}

pub fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or_default(|(_, e)| e.to_ascii_lowercase())
}

pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Parent directory of a repo-relative path ("" at the root).
pub fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

/// `dir` and its ancestors up to the root (""), nearest first.
pub fn ancestors(dir: &str) -> Vec<String> {
    let mut out = vec![dir.to_string()];
    let mut cur = dir;
    while !cur.is_empty() {
        cur = parent(cur);
        out.push(cur.to_string());
    }
    out
}

pub fn in_skip_dir(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts.iter().any(|seg| SKIP_DIRS.iter().any(|s| seg.eq_ignore_ascii_case(s)))
}

/// `root/rel` when `rel` is a plain relative path (no `..`, no drive, no root).
pub fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let candidate = Path::new(rel);
    if rel.is_empty() || rel.contains(':') {
        return None;
    }
    candidate.components().all(|c| matches!(c, Component::Normal(_))).then(|| root.join(candidate))
}

/// `path` below `dir`, compared component by component (case-insensitive),
/// so no byte offset from one string ever cuts the other.
pub fn strip_dir(path: &str, dir: &str) -> Option<String> {
    let p: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let d: Vec<&str> = dir.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    if d.is_empty() || p.len() < d.len() {
        return None;
    }
    p.iter().zip(&d).all(|(a, b)| a.to_lowercase() == b.to_lowercase()).then(|| p[d.len()..].join("/"))
}

/// Resolves `a/../b` and `./` in a repo-relative path; `None` if it leaves the root.
pub fn normalize(rel: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in rel.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

/// `target/` next to a `Cargo.toml`, `bin/` or `obj/` next to a .NET project.
fn output_dir_of(parent: &Path, name: &str) -> bool {
    if name.eq_ignore_ascii_case("target") {
        return parent.join("Cargo.toml").is_file();
    }
    if name.eq_ignore_ascii_case("bin") || name.eq_ignore_ascii_case("obj") {
        return fs::read_dir(parent)
            .is_ok_and(|entries| entries.flatten().any(|e| matches!(extension(&e.file_name().to_string_lossy()).as_str(), "csproj" | "fsproj" | "vbproj")));
    }
    false
}

/// Whether an untracked path sits in a build output folder.
pub fn is_build_output(root: &Path, rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    (0..parts.len().saturating_sub(1)).any(|i| output_dir_of(&root.join(parts[..i].join("/")), parts[i]))
}

/// Every file under `root`, skip dirs and build output pruned, repo-relative with `/`.
fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = join(&rel, &name);
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if !SKIP_DIRS.iter().any(|s| name.eq_ignore_ascii_case(s)) && !output_dir_of(&dir, &name) {
                    stack.push((entry.path(), child_rel));
                }
            } else if kind.is_file() {
                out.push(child_rel);
            }
        }
    }
    out
}

/// Glob match on `/` paths, ASCII case-insensitive: `**` spans segments,
/// `*` and `?` stay inside one.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let s: Vec<&str> = path.split('/').collect();
    match_segments(&p, &s)
}

fn match_segments(p: &[&str], s: &[&str]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((&"**", rest)) => (0..=s.len()).any(|i| match_segments(rest, &s[i..])),
        Some((head, rest)) => !s.is_empty() && match_one(head.as_bytes(), s[0].as_bytes()) && match_segments(rest, &s[1..]),
    }
}

fn match_one(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| match_one(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && match_one(rest, &s[1..]),
        Some((c, rest)) => !s.is_empty() && c.eq_ignore_ascii_case(&s[0]) && match_one(rest, &s[1..]),
    }
}

pub fn is_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_skips_output_only_next_to_its_manifest() {
        let root = std::env::temp_dir().join(format!("mt-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (p, body) in [
            ("Cargo.toml", "[package]"),
            ("src/bin/tool.rs", "fn main() {}"),
            ("target/debug/out.rs", ""),
            ("lib/target/keep.rs", ""),
            ("app/App.csproj", "<Project />"),
            ("app/obj/x.cs", ""),
            ("app/bin/y.cs", ""),
            ("node_modules/m/i.js", ""),
        ] {
            let f = root.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, body).unwrap();
        }
        let mut files = walk(&root);
        files.sort();
        assert_eq!(files, vec!["Cargo.toml", "app/App.csproj", "lib/target/keep.rs", "src/bin/tool.rs"]);
        assert!(is_build_output(&root, "target/debug/out.rs") && is_build_output(&root, "app/obj/x.cs"));
        assert!(!is_build_output(&root, "src/bin/tool.rs") && !is_build_output(&root, "lib/target/keep.rs"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lines_contain() {
        let l = Lines::Ranges(vec![(2, 4), (9, 9)]);
        assert!(!l.contains(1));
        assert!(l.contains(2) && l.contains(4) && l.contains(9));
        assert!(!l.contains(5));
        assert_eq!(l.count(), Some(4));
        assert!(Lines::All.contains(12345));
    }

    #[test]
    fn globs() {
        assert!(glob_match("demo-tool/src/*.rs", "demo-tool/src/main.rs"));
        assert!(!glob_match("demo-tool/src/*.rs", "demo-tool/src/sub/x.rs"));
        assert!(glob_match("**/*.rs", "a/b/c.rs"));
        assert!(glob_match("**/*.rs", "c.rs"));
        assert!(glob_match("SRC/?.CS", "src/a.cs"));
    }

    #[test]
    fn paths() {
        assert_eq!(normalize("./a/../b//c.rs").as_deref(), Some("b/c.rs"));
        assert_eq!(normalize("../x"), None);
        assert!(safe_join(Path::new("C:/t"), "../x").is_none());
        assert!(safe_join(Path::new("C:/t"), "C:/x").is_none());
        assert!(safe_join(Path::new("C:/t"), "a/b.rs").is_some());
        assert!(in_skip_dir("Node_Modules/a.js") && in_skip_dir("a/.git/x"));
        assert!(!in_skip_dir("src/bin/tool.rs") && !in_skip_dir("src/obj/x.cs") && !in_skip_dir("target/x.rs"));
        assert_eq!(strip_dir("C:/Users/x/mt-rev-ş/İİ.py", "c:\\users\\x\\MT-REV-ş").as_deref(), Some("İİ.py"));
        assert_eq!(strip_dir("C:/a/b", "C:/a/b").as_deref(), Some(""));
        assert_eq!(strip_dir("C:/ab/c", "C:/a"), None);
        assert_eq!(ancestors("a/b"), vec!["a/b", "a", ""]);
        assert_eq!(extension("dir.v2/File.TSX"), "tsx");
    }
}
