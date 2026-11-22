//! modern-tara: finds old constructs on the changed lines of a repository.
//!
//!   modern-tara scan --repo <dir> [--range a..b | --staged] [--files f...] [--all-lines]
//!
//! Detects each stack the change touches and its version pins, runs that
//! stack's modernizer rules on a temp copy (never in the repository), keeps
//! findings on changed lines and prints JSON (stdout) plus a summary (stderr).
//! Findings that need a newer toolchain or pin are `needs-newer`, with the
//! upgrade that unlocks them.

mod change;
mod cli;
mod copy;
mod git;
mod pins;
mod proc;
mod report;
mod stacks;
mod toolset;

use change::{Change, Lines, Source};
use git::Git;
use report::{Finding, StackReport, Status};
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let code = match cli::parse(&args) {
        Ok(cli::Command::Help) => {
            println!("{}", cli::USAGE);
            0
        }
        Ok(cli::Command::Version) => {
            print!("{}", versions());
            0
        }
        Ok(cli::Command::Scan(opts)) => match scan(&opts) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("modern-tara: {e}");
                2
            }
        },
        Err(e) => {
            eprintln!("modern-tara: {e}\n{}", cli::USAGE);
            2
        }
    };
    // Every guard (temp folders) has dropped by now.
    std::process::exit(code);
}

/// The modern-tara folder holding `js/`, `rules/`, `ps/`, `sgconfig.yml` and
/// `tools.toml`: `MODERN_TARA_HOME`, else the nearest ancestor of the
/// executable that has them (the release folder, or the repository for
/// `target/<profile>/modern-tara`), else the executable's own folder.
fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("MODERN_TARA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    let exe = std::env::current_exe().ok();
    if let Some(exe) = &exe {
        for dir in exe.ancestors().skip(1).take(5) {
            if dir.join("sgconfig.yml").is_file() && dir.join("js").is_dir() {
                return dir.to_path_buf();
            }
        }
    }
    exe.and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default()
}

fn git_exe() -> Option<PathBuf> {
    proc::which("git", &stacks::fallbacks("git"))
}

fn scan(o: &cli::Opts) -> Result<i32, String> {
    let started = Instant::now();
    // The long, real-case form, as git prints the top level: `--repo` under an
    // 8.3 TEMP (`RUNNER~1`) or in another case must still give the subfolder.
    let dir = copy::real_path(&std::path::absolute(&o.repo).map_err(|e| format!("{}: {e}", o.repo.display()))?);
    if !dir.is_dir() {
        return Err(format!("{}: not a folder", dir.display()));
    }
    let change = build_change(o, &dir)?;
    let temp = copy::TempRoot::create()?;
    let ctx = stacks::Ctx { change: &change, temp: &temp, home: home(), tools: stacks::Tools::find(), timeout: o.timeout };

    let selected: Vec<&'static str> = stacks::ALL.iter().copied().filter(|s| o.stacks.as_ref().is_none_or(|l| l.iter().any(|x| x == s))).collect();
    let work: Vec<(&'static str, Vec<String>)> =
        selected.into_iter().map(|s| (s, change.files_with(stacks::extensions(s)))).filter(|(_, f)| !f.is_empty()).collect();

    let mut reports: Vec<StackReport> = std::thread::scope(|scope| {
        let handles: Vec<_> = work
            .iter()
            .map(|(stack, files)| {
                let ctx = &ctx;
                (*stack, files, scope.spawn(move || run_stack(ctx, stack, files)))
            })
            .collect();
        handles
            .into_iter()
            .map(|(stack, files, h)| {
                h.join().unwrap_or_else(|_| {
                    let mut r = StackReport::new(stack, files.clone());
                    r.fail("internal error (panic) in this stack");
                    r
                })
            })
            .collect()
    });

    let mut findings: Vec<Finding> = Vec::new();
    for r in &mut reports {
        for f in r.findings.drain(..) {
            if change.files.get(&f.file).is_some_and(|l| l.contains(f.line)) {
                findings.push(f);
            }
        }
    }
    report::tidy(&mut findings);
    drop(temp);

    let repo = change.root.to_string_lossy().replace('\\', "/");
    let doc = report::document(VERSION, &repo, &change.describe, &reports, &findings);
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    if let Some(path) = &o.json_out {
        std::fs::write(path, format!("{text}\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    } else {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
    let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let failed = reports.iter().any(|r| r.status == Status::Error);
    let code = report::exit_code(&findings, failed, o.gate);
    if !o.quiet {
        eprint!("{}", report::summary(&reports, &findings, total_ms, o.gate.then_some(code)));
    }
    Ok(code)
}

fn run_stack(ctx: &stacks::Ctx, stack: &'static str, files: &[String]) -> StackReport {
    let t = Instant::now();
    let mut r = match stack {
        "csharp" => stacks::csharp::run(ctx, files),
        "rust" => stacks::rust::run(ctx, files),
        "python" => stacks::python::run(ctx, files),
        "js" => stacks::js::run_js(ctx, files),
        "css" => stacks::js::run_css(ctx, files),
        "powershell" => stacks::powershell::run(ctx, files),
        "go" => stacks::go::run(ctx, files),
        _ => StackReport::new(stack, files.to_vec()),
    };
    r.duration_ms = u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX);
    r
}

/// Turns the options into the files and lines to scan, and their source.
fn build_change(o: &cli::Opts, dir: &Path) -> Result<Change, String> {
    let git = git_exe();
    let top = match &git {
        Some(exe) => Git::toplevel(exe, dir, o.git_safe)?,
        None => None,
    };
    let Some(top) = top else {
        if !o.all_lines {
            return Err(format!("{} is not a git repository: use --files <f>... --all-lines (or --all-lines alone)", dir.display()));
        }
        if o.range.is_some() || o.staged {
            return Err("--range and --staged need a git repository".into());
        }
        let mut change = Change::new(dir.to_path_buf(), Source::Dir, BTreeMap::new(), json!({}));
        let files = select(&change, &o.files, "", dir)?;
        let count = files.len();
        change.files = files.into_iter().map(|f| (f, Lines::All)).collect();
        change.describe = json!({"mode": "files", "git": false, "all_lines": true, "files": count});
        return Ok(change);
    };
    let exe = git.ok_or("git not found")?;
    let g = Git { exe, repo: top.clone(), safe: o.git_safe };
    // `--files` paths are relative to --repo, which may be below the top level.
    let prefix = change::strip_dir(&dir.to_string_lossy(), &top.to_string_lossy()).unwrap_or_default();

    let (source, base, target, mode) = if let Some((a, b)) = &o.range {
        let from = g.resolve(a)?;
        let to = g.resolve(b)?;
        (Source::Rev(g.clone(), to.clone()), from, Some(to), "range")
    } else if o.staged {
        let from = if g.has_head() { "HEAD".to_string() } else { git::EMPTY_TREE.to_string() };
        (Source::Index(g.clone()), from, Some(String::new()), "staged")
    } else {
        let from = if g.has_head() { "HEAD".to_string() } else { git::EMPTY_TREE.to_string() };
        (Source::WorkTree(g.clone()), from, None, "worktree")
    };
    let mut change = Change::new(top.clone(), source, BTreeMap::new(), json!({}));
    let wanted: Option<Vec<String>> = if o.files.is_empty() { None } else { Some(select(&change, &o.files, &prefix, dir)?) };
    let files: BTreeMap<String, Lines> = if o.all_lines {
        let list = match &wanted {
            Some(w) => w.clone(),
            None => match &change.source {
                Source::WorkTree(g) => g.ls_index()?.into_iter().filter(|p| !change::in_skip_dir(p)).collect(),
                // Tracked files are sources even under bin/ or target/.
                _ => change.listing()?.to_vec(),
            },
        };
        list.into_iter().map(|f| (f, Lines::All)).collect()
    } else {
        let mut map: BTreeMap<String, Lines> = g.diff(&base, target.as_deref(), &[])?.into_iter().map(|(f, r)| (f, Lines::Ranges(r))).collect();
        if mode == "worktree" {
            for u in g.ls_untracked()? {
                map.entry(u).or_insert(Lines::All);
            }
        }
        if let Some(w) = &wanted {
            map.retain(|f, _| w.contains(f));
        }
        map
    };
    // `--repo <subdir>` without `--files`: only that subdir.
    let mut files = files;
    if wanted.is_none() && !prefix.is_empty() {
        files.retain(|f, _| change::strip_dir(f, &prefix).is_some_and(|rest| !rest.is_empty()));
    }
    let changed_lines: u64 = files.values().filter_map(Lines::count).sum();
    change.describe = json!({
        "mode": mode,
        "from": if mode == "range" || !o.all_lines { Some(base) } else { None },
        "to": match mode { "range" => target, "staged" => Some("index".into()), _ => Some("worktree".into()) },
        "all_lines": o.all_lines,
        "files": files.len(),
        "changed_lines": if o.all_lines { None } else { Some(changed_lines) },
        "git_safe": o.git_safe,
    });
    change.files = files;
    Ok(change)
}

/// `--files` patterns resolved against the source's listing (globs) or as
/// plain paths, relative to the `--repo` folder.
fn select(change: &Change, patterns: &[String], prefix: &str, dir: &Path) -> Result<Vec<String>, String> {
    let listing = change.listing()?;
    let dir_text = dir.to_string_lossy();
    let mut out = Vec::new();
    for raw in patterns {
        let mut p = raw.replace('\\', "/");
        if Path::new(raw).is_absolute() {
            let real = copy::real_path(Path::new(raw)).to_string_lossy().replace('\\', "/");
            p = change::strip_dir(&real, &dir_text)
                .or_else(|| change::strip_dir(&p, &dir_text))
                .ok_or_else(|| format!("--files {raw}: outside {}", dir.display()))?;
        }
        let p = p.trim_start_matches("./");
        let full = if prefix.is_empty() { p.to_string() } else { format!("{prefix}/{p}") };
        if change::is_glob(&full) {
            let before = out.len();
            out.extend(listing.iter().filter(|l| change::glob_match(&full, l)).cloned());
            if out.len() == before {
                return Err(format!("--files {raw}: matches nothing"));
            }
        } else {
            let n = change::normalize(&full).ok_or_else(|| format!("--files {raw}: outside the repository"))?;
            match listing.iter().find(|l| l.eq_ignore_ascii_case(&n)) {
                Some(l) => out.push(l.clone()),
                None if matches!(change.source, Source::WorkTree(_) | Source::Dir) && change.root.join(&n).is_file() => out.push(n),
                None => return Err(format!("--files {raw}: no such file")),
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// `--version`: this tool and every underlying tool, found or not, then a
/// warning line for each tool whose version differs from `tools.toml`.
fn versions() -> String {
    let tools = stacks::Tools::find();
    let home = home();
    let temp = std::env::temp_dir();
    let run = |exe: &Option<PathBuf>, args: &[&str]| -> String {
        exe.as_ref().map_or_else(
            || "not found".to_string(),
            |e| {
                proc::Cmd::new(e)
                    .args(args)
                    .cwd(&temp)
                    .env("DOTNET_NOLOGO", "1")
                    // `go version` must not download the toolchain a GOTOOLCHAIN value names.
                    .env("GOTOOLCHAIN", "local")
                    .run()
                    .ok()
                    .filter(proc::Output::ok)
                    .map_or_else(|| "failed".to_string(), |o| o.stdout_text().lines().next().unwrap_or("").trim().to_string())
            },
        )
    };
    let pkg = |name: &str| -> String {
        let path = home.join("js").join("node_modules").join(name).join("package.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.get("version").and_then(|x| x.as_str()).map(String::from))
            .unwrap_or_else(|| "not installed".into())
    };
    let pssa_query = "(Get-Module -ListAvailable PSScriptAnalyzer | Sort-Object Version -Descending | Select-Object -First 1).Version.ToString()";
    let lines: Vec<(&str, String)> = std::thread::scope(|s| {
        let jobs: Vec<(&str, std::thread::ScopedJoinHandle<String>)> = vec![
            ("git", s.spawn(|| run(&git_exe(), &["--version"]))),
            ("rustc", s.spawn(|| run(&tools.rustc, &["--version"]))),
            ("clippy", s.spawn(|| run(&tools.cargo, &["clippy", "--version"]))),
            ("dotnet sdk", s.spawn(|| run(&tools.dotnet, &["--version"]))),
            ("dotnet format", s.spawn(|| run(&tools.dotnet, &["format", "--version"]))),
            ("ruff", s.spawn(|| run(&tools.ruff, &["--version"]))),
            ("node", s.spawn(|| run(&tools.node, &["--version"]))),
            ("pwsh", s.spawn(|| run(&tools.pwsh, &["--version"]))),
            ("PSScriptAnalyzer", s.spawn(|| run(&tools.pwsh, &["-NoProfile", "-NonInteractive", "-Command", pssa_query]))),
            ("ast-grep", s.spawn(|| run(&tools.ast_grep, &["--version"]))),
            ("go", s.spawn(|| run(&tools.go, &["version"]))),
        ];
        jobs.into_iter().map(|(n, h)| (n, h.join().unwrap_or_else(|_| "failed".into()))).collect()
    });
    let mut out = format!("modern-tara {VERSION}\n");
    for (name, v) in &lines {
        let _ = writeln!(out, "  {name:<28} {v}");
    }
    for name in [
        "eslint",
        "eslint-plugin-unicorn",
        "typescript-eslint",
        "typescript",
        "eslint-plugin-es-x",
        "@eslint/css",
        "stylelint",
        "stylelint-plugin-use-baseline",
    ] {
        let _ = writeln!(out, "  {name:<28} {}", pkg(name));
    }
    let _ = writeln!(out, "  {:<28} {}", "home", home.display());
    match toolset::load(&home) {
        Ok(set) => {
            for (name, pinned) in &set.tools {
                let installed = lines.iter().find(|(n, _)| n == name).map_or("not checked", |(_, v)| v.as_str());
                if let Some(w) = toolset::check(name, pinned, installed) {
                    let _ = writeln!(out, "{w}");
                }
            }
            for (name, pinned) in &set.npm {
                if let Some(w) = toolset::check(name, pinned, &pkg(name)) {
                    let _ = writeln!(out, "{w}");
                }
            }
        }
        Err(e) => {
            let _ = writeln!(out, "warning: {e}");
        }
    }
    out
}
