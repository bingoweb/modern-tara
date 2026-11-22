//! Command-line options.

use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "usage: modern-tara scan [--repo <dir>] [--range <a>..<b> | --staged] [--files <f>...] [--all-lines]
                        [--stacks csharp,rust,python,js,css,powershell,go] [--json <out.json>] [--quiet]
                        [--git-safe] [--timeout <seconds>] [--gate]
       modern-tara --version

  --range a..b  changed lines of `git diff -U0 a b`, contents from revision b
  --staged      changes in the index against HEAD
  (neither)     work tree against HEAD, untracked files whole
  --files       only these paths (globs allowed); with --all-lines, whole files,
                also in a folder that is not a git repository
  --all-lines   whole files (every tracked file when no --files)
  --json FILE   write the JSON report to FILE instead of stdout
  --quiet       no summary on stderr
  --git-safe    trust a repository owned by another user (safe.directory, per command)
  --timeout S   per-stack timeout in seconds (defaults: rust 900, csharp 600, js 300, go 600 for the whole stack, others 120-180)
  --gate        exit 1 only for high-confidence `modern` findings; the others are warnings

exit: 0 no findings, 1 findings, 2 tool error (or a stack failed and nothing was found)
      with --gate: 1 only for high-confidence `modern` findings, 2 a stack failed and none was found";

#[derive(Debug, Default, Clone)]
#[expect(clippy::struct_excessive_bools, reason = "one field per command-line switch")]
pub struct Opts {
    pub repo: PathBuf,
    pub range: Option<(String, String)>,
    pub staged: bool,
    pub files: Vec<String>,
    pub all_lines: bool,
    pub stacks: Option<Vec<String>>,
    pub json_out: Option<PathBuf>,
    pub quiet: bool,
    pub git_safe: bool,
    pub timeout: Option<Duration>,
    /// `--gate`: only high-confidence `modern` findings make exit 1.
    pub gate: bool,
}

pub enum Command {
    Scan(Opts),
    Version,
    Help,
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let Some(first) = args.first() else { return Ok(Command::Help) };
    match first.as_str() {
        "--version" | "-V" | "version" => return Ok(Command::Version),
        "--help" | "-h" | "help" => return Ok(Command::Help),
        "scan" => {}
        other => return Err(format!("unknown command: {other}")),
    }
    let mut o = Opts { repo: PathBuf::from("."), ..Opts::default() };
    let mut i = 1;
    let value = |i: &mut usize, name: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i).filter(|v| !v.starts_with("--")).cloned().ok_or_else(|| format!("{name} needs a value"))
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--repo" => o.repo = PathBuf::from(value(&mut i, a)?),
            "--range" => {
                let v = value(&mut i, a)?;
                let (from, to) = v.split_once("..").filter(|(f, t)| !f.is_empty() && !t.is_empty() && !t.starts_with('.')).ok_or("--range wants <a>..<b>")?;
                o.range = Some((from.to_string(), to.to_string()));
            }
            "--staged" => o.staged = true,
            "--all-lines" => o.all_lines = true,
            "--quiet" | "-q" => o.quiet = true,
            "--git-safe" => o.git_safe = true,
            "--gate" => o.gate = true,
            "--json" => o.json_out = Some(PathBuf::from(value(&mut i, a)?)),
            "--timeout" => {
                let secs: u64 = value(&mut i, a)?.parse().map_err(|_| "--timeout wants seconds")?;
                o.timeout = Some(Duration::from_secs(secs.max(1)));
            }
            "--stacks" => {
                let v = value(&mut i, a)?;
                let list: Vec<String> = v.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
                for s in &list {
                    if !crate::stacks::ALL.contains(&s.as_str()) {
                        return Err(format!("unknown stack: {s} (known: {})", crate::stacks::ALL.join(",")));
                    }
                }
                o.stacks = Some(list);
            }
            "--files" => {
                while args.get(i + 1).is_some_and(|v| !v.starts_with("--")) {
                    i += 1;
                    o.files.push(args[i].clone());
                }
                if o.files.is_empty() {
                    return Err("--files needs at least one path".into());
                }
            }
            other => return Err(format!("unknown option: {other}")),
        }
        i += 1;
    }
    if o.range.is_some() && o.staged {
        return Err("--range and --staged exclude each other".into());
    }
    Ok(Command::Scan(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(args: &[&str]) -> Opts {
        let v: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        match parse(&v).unwrap() {
            Command::Scan(o) => o,
            _ => panic!("not scan"),
        }
    }

    #[test]
    fn options() {
        let o = scan(&["scan", "--repo", "C:/r", "--range", "abc..HEAD", "--files", "a/*.rs", "b.rs", "--all-lines", "--stacks", "rust,python", "--quiet"]);
        assert_eq!(o.range, Some(("abc".into(), "HEAD".into())));
        assert_eq!(o.files, vec!["a/*.rs", "b.rs"]);
        assert!(o.all_lines && o.quiet && !o.gate);
        assert!(scan(&["scan", "--gate"]).gate);
        assert_eq!(o.stacks, Some(vec!["rust".into(), "python".into()]));
        assert_eq!(scan(&["scan", "--stacks", "Go,rust"]).stacks, Some(vec!["go".into(), "rust".into()]));
    }

    #[test]
    fn bad_options() {
        let bad = |a: &[&str]| parse(&a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>()).is_err();
        assert!(bad(&["scan", "--range", "a...b"]));
        assert!(bad(&["scan", "--range", "a.."]));
        assert!(bad(&["scan", "--stacks", "cobol"]));
        assert!(bad(&["scan", "--range", "a..b", "--staged"]));
        assert!(bad(&["scan", "--files"]));
        assert!(bad(&["lint"]));
    }
}
