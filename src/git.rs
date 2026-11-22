//! Read-only git: diffs, file lists and blob contents. Every call passes
//! `--no-optional-locks` (no opportunistic index refresh), turns fsmonitor off
//! and never runs in a mode that writes to the repository.

use crate::proc::{Cmd, Output};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The empty tree, for diffs in a repository without commits.
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Clone)]
pub struct Git {
    pub exe: PathBuf,
    pub repo: PathBuf,
    /// Adds `-c safe.directory=<repo>` (repositories owned by another user).
    pub safe: bool,
}

/// Changed new-side line ranges per repo-relative path, inclusive and 1-based.
pub type LineMap = BTreeMap<String, Vec<(u32, u32)>>;

impl Git {
    pub fn cmd(&self) -> Cmd {
        let mut cmd = Cmd::new(&self.exe)
            .args(["--no-optional-locks", "-c", "core.quotepath=false", "-c", "core.fsmonitor=false"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .timeout(Duration::from_secs(120));
        if self.safe {
            let dir = self.repo.to_string_lossy().replace('\\', "/");
            cmd = cmd.arg("-c").arg(format!("safe.directory={dir}"));
        }
        cmd.arg("-C").arg(&self.repo)
    }

    fn run(&self, args: &[&str]) -> Result<Output, String> {
        let out = self.cmd().args(args).run()?;
        if out.ok() {
            Ok(out)
        } else {
            let tail = out.tail(3);
            let hint = if tail.contains("safe.directory") { " (owned by another user: pass --git-safe)" } else { "" };
            Err(format!("git {} failed: {tail}{hint}", args.first().copied().unwrap_or_default()))
        }
    }

    /// The top level of the work tree containing `dir`, or `None` outside git.
    pub fn toplevel(exe: &Path, dir: &Path, safe: bool) -> Result<Option<PathBuf>, String> {
        let probe = Git { exe: exe.to_path_buf(), repo: dir.to_path_buf(), safe };
        let out = probe.cmd().args(["rev-parse", "--show-toplevel"]).run()?;
        if out.ok() {
            return Ok(top_path(&out.stdout_text()));
        }
        let tail = out.tail(3);
        if tail.contains("safe.directory") || tail.contains("dubious ownership") {
            return Err(format!("{}: owned by another user: pass --git-safe", dir.display()));
        }
        Ok(None)
    }

    pub fn has_head(&self) -> bool {
        self.cmd().args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).run().is_ok_and(|o| o.ok())
    }

    pub fn resolve(&self, rev: &str) -> Result<String, String> {
        if rev.starts_with('-') {
            return Err(format!("not a revision: {rev}"));
        }
        let spec = format!("{rev}^{{commit}}");
        let out = self.run(&["rev-parse", "--verify", "--quiet", "--end-of-options", &spec])?;
        Ok(out.stdout_text().trim().to_string())
    }

    /// `git diff -U0` between `from` and `to` (`None` = work tree, `Some("")`
    /// = index), restricted to `paths` when given.
    pub fn diff(&self, from: &str, to: Option<&str>, paths: &[String]) -> Result<LineMap, String> {
        let mut args: Vec<String> =
            ["diff", "-U0", "--no-color", "--no-ext-diff", "--no-textconv", "-M", "--src-prefix=a/", "--dst-prefix=b/", "--ignore-submodules"]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
        match to {
            Some("") => {
                args.push("--cached".into());
                args.push(from.into());
            }
            Some(b) => {
                args.push(from.into());
                args.push(b.into());
            }
            None => args.push(from.into()),
        }
        args.push("--".into());
        args.extend(paths.iter().cloned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.run(&refs)?;
        Ok(parse_diff(&String::from_utf8_lossy(&out.stdout)))
    }

    /// NUL-separated output split into paths.
    fn list(&self, args: &[&str]) -> Result<Vec<String>, String> {
        let out = self.run(args)?;
        Ok(out.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
    }

    pub fn ls_tree(&self, rev: &str) -> Result<Vec<String>, String> {
        self.list(&["ls-tree", "-r", "-z", "--name-only", "--full-tree", rev])
    }
    pub fn ls_index(&self) -> Result<Vec<String>, String> {
        self.list(&["ls-files", "-z", "--cached"])
    }
    pub fn ls_untracked(&self) -> Result<Vec<String>, String> {
        self.list(&["ls-files", "-z", "--others", "--exclude-standard"])
    }

    /// Blob contents for `<prefix><path>` specs (`rev:` or `:` for the index).
    /// `cat-file --batch-check` first, so blobs above `max` bytes are never
    /// read; then one `cat-file --batch`. Missing objects are left out.
    pub fn read_blobs(&self, prefix: &str, paths: &[String], max: usize) -> Result<HashMap<String, Vec<u8>>, String> {
        let mut found = HashMap::new();
        let specs: Vec<&String> = paths.iter().filter(|p| !p.contains('\n')).collect();
        if specs.is_empty() {
            return Ok(found);
        }
        let request = |list: &[&String]| -> Vec<u8> { list.iter().flat_map(|p| format!("{prefix}{p}\n").into_bytes()).collect() };
        let check = self.cmd().args(["cat-file", "--batch-check"]).stdin(request(&specs)).run()?;
        let sizes = String::from_utf8_lossy(&check.stdout).into_owned();
        let wanted: Vec<&String> = specs
            .iter()
            .zip(sizes.lines())
            .filter(|(_, line)| {
                let parts: Vec<&str> = line.split(' ').collect();
                parts.len() == 3 && parts[1] == "blob" && parts[2].parse::<usize>().is_ok_and(|n| n <= max)
            })
            .map(|(p, _)| *p)
            .collect();
        if wanted.is_empty() {
            return Ok(found);
        }
        let out = self.cmd().args(["cat-file", "--batch"]).stdin(request(&wanted)).run()?;
        let bytes = out.stdout;
        let mut at = 0usize;
        for path in wanted {
            let Some(nl) = bytes[at..].iter().position(|b| *b == b'\n') else { break };
            let header = String::from_utf8_lossy(&bytes[at..at + nl]).into_owned();
            at += nl + 1;
            let parts: Vec<&str> = header.trim_end().split(' ').collect();
            if parts.len() != 3 {
                continue;
            }
            let size: usize = parts[2].parse().map_err(|_| format!("git cat-file: bad header {header}"))?;
            let Some(body) = bytes.get(at..at + size) else { break };
            if parts[1] == "blob" {
                found.insert(path.clone(), body.to_vec());
            }
            at += size + 1;
        }
        Ok(found)
    }
}

/// Parses `git diff -U0` output into new-side changed line ranges. Hunk bodies
/// are skipped by count, so content lines that look like headers are safe.
/// Pure deletions add nothing; deleted files (`+++ /dev/null`) are left out.
pub fn parse_diff(text: &str) -> LineMap {
    let mut map: LineMap = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut old_left: u64 = 0;
    let mut new_left: u64 = 0;
    for raw in text.split('\n') {
        if old_left > 0 || new_left > 0 {
            match raw.as_bytes().first() {
                Some(b'-') if old_left > 0 => {
                    old_left -= 1;
                    continue;
                }
                Some(b'+') if new_left > 0 => {
                    new_left -= 1;
                    continue;
                }
                Some(b'\\') => continue,
                _ => {
                    old_left = 0;
                    new_left = 0;
                }
            }
        }
        let line = raw.trim_suffix('\r');
        if line.starts_with("diff --git ") {
            current = None;
        } else if let Some(path) = line.strip_prefix("+++ ") {
            current = diff_path(path);
            if let Some(p) = &current {
                map.entry(p.clone()).or_default();
            }
        } else if line.starts_with("@@ ")
            && let Some((old_count, new_start, new_count)) = parse_hunk(line)
        {
            old_left = old_count;
            new_left = new_count;
            if new_count > 0
                && let Some(path) = &current
            {
                let end = new_start.saturating_add(u32::try_from(new_count - 1).unwrap_or(u32::MAX));
                map.entry(path.clone()).or_default().push((new_start, end));
            }
        }
    }
    map.retain(|_, ranges| !ranges.is_empty());
    map
}

/// `@@ -a[,b] +c[,d] @@` into `(b, c, d)`.
fn parse_hunk(line: &str) -> Option<(u64, u32, u64)> {
    let inner = line.strip_prefix("@@ ")?;
    let end = inner.find(" @@")?;
    let mut parts = inner[..end].split(' ');
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let count = |spec: &str| -> Option<(u64, u64)> {
        match spec.split_once(',') {
            Some((start, n)) => Some((start.parse().ok()?, n.parse().ok()?)),
            None => Some((spec.parse().ok()?, 1)),
        }
    };
    let (_, old_count) = count(old)?;
    let (new_start, new_count) = count(new)?;
    Some((old_count, u32::try_from(new_start).ok()?, new_count))
}

/// The path of a `+++` line: `b/` prefix removed, C quoting undone, and the
/// trailing tab git adds to names containing spaces dropped.
fn diff_path(spec: &str) -> Option<String> {
    if spec == "/dev/null" {
        return None;
    }
    let path = if spec.starts_with('"') { unquote(spec)? } else { spec.trim_suffix('\t').to_string() };
    Some(path.trim_prefix("b/").to_string())
}

/// Git's C-style quoting: `\t \n \" \\` and octal byte escapes.
pub fn unquote(spec: &str) -> Option<String> {
    let body = spec.strip_prefix('"')?;
    let end = body.rfind('"')?;
    let body = &body.as_bytes()[..end];
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let b = body[i];
        if b != b'\\' {
            out.push(b);
            i += 1;
            continue;
        }
        let next = *body.get(i + 1)?;
        i += 2;
        match next {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'v' => out.push(0x0b),
            b'0'..=b'7' => {
                let digits = &body[i - 1..(i + 2).min(body.len())];
                let value = digits.iter().take(3).try_fold(0u32, |acc, d| (b'0'..=b'7').contains(d).then(|| acc * 8 + u32::from(d - b'0')))?;
                out.push(u8::try_from(value).ok()?);
                i += 2;
            }
            other => out.push(other),
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// `git rev-parse --show-toplevel` output as a path, unchanged: Windows takes
/// `/` as a separator, and a POSIX path must keep its leading `/`.
fn top_path(text: &str) -> Option<PathBuf> {
    let top = text.trim();
    (!top.is_empty()).then(|| PathBuf::from(top))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toplevel_paths_stay_as_git_prints_them() {
        let posix = top_path("/srv/repo\n").unwrap();
        assert_eq!(posix.to_string_lossy(), "/srv/repo");
        assert!(posix.has_root(), "a POSIX top level must stay absolute");
        #[cfg(unix)]
        assert!(posix.is_absolute());
        assert_eq!(top_path("C:/src/repo\n").unwrap().to_string_lossy(), "C:/src/repo");
        assert_eq!(top_path("  \n"), None);
    }
    #[test]
    fn adds_and_counts() {
        let d = "diff --git a/x.rs b/x.rs\nindex 1..2 100644\n--- a/x.rs\n+++ b/x.rs\n@@ -2 +2 @@ fn a\n-old\n+new\n@@ -3,0 +4,3 @@\n+a\n+b\n+c\n";
        let m = parse_diff(d);
        assert_eq!(m["x.rs"], vec![(2, 2), (4, 6)]);
    }

    #[test]
    fn pure_deletion_and_deleted_file_add_nothing() {
        let d = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -5,2 +4,0 @@\n-gone\n-gone\n\
                 diff --git a/y.rs b/y.rs\ndeleted file mode 100644\n--- a/y.rs\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n";
        assert!(parse_diff(d).is_empty());
    }

    #[test]
    fn rename_uses_new_path() {
        let d = "diff --git a/old.py b/new.py\nsimilarity index 50%\nrename from old.py\nrename to new.py\n--- a/old.py\n+++ b/new.py\n@@ -1,0 +2 @@ q\n+r\n";
        assert_eq!(parse_diff(d)["new.py"], vec![(2, 2)]);
    }

    #[test]
    fn crlf_lines_and_header_lookalikes_in_bodies() {
        let d = "diff --git a/a.cs b/a.cs\r\n--- a/a.cs\r\n+++ b/a.cs\r\n@@ -1,2 +1,2 @@\r\n--- a/evil\r\n-x\r\n+++ b/evil\r\n+@@ -9 +9 @@\r\n";
        let m = parse_diff(d);
        assert_eq!(m.len(), 1);
        assert_eq!(m["a.cs"], vec![(1, 2)]);
    }

    #[test]
    fn header_lookalike_then_second_hunk_stays_with_its_file() {
        // Without skip-by-count, `+++ b/evil` would switch files and the
        // second hunk would land on "evil".
        let d = "diff --git a/a.cs b/a.cs\n--- a/a.cs\n+++ b/a.cs\n@@ -1,2 +1,2 @@\n--- a/evil\n-x\n+++ b/evil\n+y\n@@ -9 +9 @@\n-p\n+q\n";
        let m = parse_diff(d);
        assert_eq!(m.keys().collect::<Vec<_>>(), vec!["a.cs"]);
        assert_eq!(m["a.cs"], vec![(1, 2), (9, 9)]);
    }

    #[test]
    fn no_newline_marker_inside_a_hunk_keeps_counting() {
        // The marker sits between the removed and the added line; treating it
        // as the end of the hunk would read `+++ b/evil` as a header.
        let d = "--- a/a.cs\n+++ b/a.cs\n@@ -1 +1 @@\n-x\n\\ No newline at end of file\n+++ b/evil\n\\ No newline at end of file\n@@ -5 +5 @@\n-p\n+q\n";
        let m = parse_diff(d);
        assert_eq!(m.keys().collect::<Vec<_>>(), vec!["a.cs"]);
        assert_eq!(m["a.cs"], vec![(1, 1), (5, 5)]);
    }

    #[test]
    fn turkish_and_spaced_and_quoted_names() {
        let d = "--- a/ğüşıöç.txt\n+++ b/ğüşıöç.txt\n@@ -2 +2,2 @@ x\n-y\n+Y\n+z\n\
                 --- a/with space.txt\t\n+++ b/with space.txt\t\n@@ -3,0 +4 @@ c\n+d\n\
                 --- \"a/t\\tab.txt\"\n+++ \"b/t\\tab.txt\"\n@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file\n\
                 --- \"a/\\303\\274.txt\"\n+++ \"b/\\303\\274.txt\"\n@@ -1 +1 @@\n-a\n+b\n";
        let m = parse_diff(d);
        assert_eq!(m["ğüşıöç.txt"], vec![(2, 3)]);
        assert_eq!(m["with space.txt"], vec![(4, 4)]);
        assert_eq!(m["t\tab.txt"], vec![(1, 1)]);
        assert_eq!(m["ü.txt"], vec![(1, 1)]);
    }

    #[test]
    fn binary_and_mode_only_changes_are_skipped() {
        let d = "diff --git a/i.png b/i.png\nBinary files a/i.png and b/i.png differ\ndiff --git a/s.sh b/s.sh\nold mode 100644\nnew mode 100755\n";
        assert!(parse_diff(d).is_empty());
    }

    #[test]
    fn hunk_header_forms() {
        assert_eq!(parse_hunk("@@ -1 +1 @@"), Some((1, 1, 1)));
        assert_eq!(parse_hunk("@@ -10,0 +11,4 @@ ctx"), Some((0, 11, 4)));
        assert_eq!(parse_hunk("@@ -3,2 +2,0 @@"), Some((2, 2, 0)));
        assert_eq!(parse_hunk("@@ junk"), None);
    }
}
