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
mod waiver;

use change::{Change, Lines, Source};
use git::Git;
use report::{Finding, StackReport};
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
    let config = load_config(o, &change)?;
    let mut waivers = config.waivers;
    let temp = copy::TempRoot::create()?;
    let ctx =
        stacks::Ctx { change: &change, temp: &temp, home: home(), tools: stacks::Tools::find(), timeout: o.timeout, classic_scripts: &config.classic_scripts };

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

    // Touched files no stack can copy (unreadable or too large) are unscanned everywhere.
    let touched: Vec<String> = work.iter().flat_map(|(_, files)| files.iter().cloned()).collect();
    let unreadable = change.unreadable(&touched)?;
    for r in &mut reports {
        r.allowed = o.allow_skipped.iter().any(|s| s == r.stack);
        for f in r.files.iter().filter(|f| unreadable.contains(f)).cloned().collect::<Vec<_>>() {
            r.unscanned(f, format!("unreadable or over {} MiB: not copied", change::MAX_FILE_BYTES >> 20));
        }
    }
    let no_stack = no_stack(&change);

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
    let today = waiver::today_utc();
    let (findings, waived) = waivers.apply(findings, today);
    let mut warnings = unmatched_warnings(&change);
    warnings.extend(reports.iter().flat_map(|r| r.warnings.iter().cloned()));
    warnings.extend(waivers.warnings(today, o.all_lines));

    let repo = change.root.to_string_lossy().replace('\\', "/");
    let extras = report::Extras { waived: &waived, waivers: &waivers.to_json(today), warnings: &warnings, no_stack: &no_stack };
    let doc = report::document(VERSION, &repo, &change.describe, &reports, &findings, &extras);
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    if let Some(path) = &o.json_out {
        std::fs::write(path, format!("{text}\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    } else {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
    let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let code = report::exit_code(&findings, &reports, o.gate);
    if !o.quiet {
        eprint!("{}", report::summary(&reports, &findings, waived.len(), &warnings, total_ms, o.gate.then_some(code), &no_stack));
    }
    Ok(code)
}

/// Changed files whose extension no stack takes, per extension.
fn no_stack(change: &Change) -> report::NoStack {
    let mut by = report::NoStack::new();
    for f in change.files.keys() {
        let ext = change::extension(f);
        if !stacks::ALL.iter().any(|s| stacks::extensions(s).contains(&ext.as_str())) {
            *by.entry(if ext.is_empty() { "(none)".into() } else { ext }).or_default() += 1;
        }
    }
    by
}

/// Folders `--all-lines` alone leaves out of a plain folder (besides
/// `change::SKIP_DIRS`); `bin` right under `src` is a Rust source folder.
const ALL_LINES_SKIP: [&str; 9] = [".git", "node_modules", "target", "bin", "obj", ".venv", "__pycache__", "dist", "build"];

fn all_lines_skipped(path: &str) -> bool {
    let dirs: Vec<&str> = path.split('/').collect();
    let dirs = &dirs[..dirs.len().saturating_sub(1)];
    dirs.iter()
        .enumerate()
        .any(|(i, d)| ALL_LINES_SKIP.iter().any(|s| d.eq_ignore_ascii_case(s)) && !(d.eq_ignore_ascii_case("bin") && i > 0 && dirs[i - 1] == "src"))
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
        let (files, unmatched) = if o.files.is_empty() {
            (change.listing()?.iter().filter(|p| !all_lines_skipped(p)).cloned().collect(), Vec::new())
        } else {
            select(&change, &o.files, "", dir)?
        };
        let count = files.len();
        change.files = files.into_iter().map(|f| (f, Lines::All)).collect();
        change.describe = with_unmatched(json!({"mode": "files", "git": false, "all_lines": true, "files": count}), &unmatched);
        return Ok(change);
    };
    let exe = git.ok_or("git not found")?;
    let g = Git { exe, repo: top.clone(), safe: o.git_safe };
    // `--files` paths are relative to --repo, which may be below the top level.
    let prefix = change::strip_dir(&dir.to_string_lossy(), &top.to_string_lossy()).unwrap_or_default();

    let (source, base, target, mode) = if let Some((a, b)) = &o.range {
        let to = g.resolve(b)?;
        let from = g.resolve(a)?;
        // Both names resolved above (neither starts with `-`); merge-base gets
        // them as typed so its error message names them.
        let from = if o.merge_base { g.merge_base(a, b)? } else { from };
        (Source::Rev(g.clone(), to.clone()), from, Some(to), "range")
    } else if o.staged {
        let from = if g.has_head() { "HEAD".to_string() } else { git::EMPTY_TREE.to_string() };
        (Source::Index(g.clone()), from, Some(String::new()), "staged")
    } else {
        let from = if g.has_head() { "HEAD".to_string() } else { git::EMPTY_TREE.to_string() };
        (Source::WorkTree(g.clone()), from, None, "worktree")
    };
    let mut change = Change::new(top.clone(), source, BTreeMap::new(), json!({}));
    let (wanted, unmatched): (Option<Vec<String>>, Vec<String>) = if o.files.is_empty() {
        (None, Vec::new())
    } else {
        let (files, unmatched) = select(&change, &o.files, &prefix, dir)?;
        (Some(files), unmatched)
    };
    let files: BTreeMap<String, Lines> = if o.all_lines {
        let list = match &wanted {
            Some(w) => w.clone(),
            // Tracked files are sources even under bin/ or target/; the work
            // tree adds untracked files that are not ignored (nor build output).
            None => change.listing()?.to_vec(),
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
    change.describe = with_unmatched(
        json!({
            "mode": mode,
            "from": if mode == "range" || !o.all_lines { Some(base) } else { None },
            "to": match mode { "range" => target, "staged" => Some("index".into()), _ => Some("worktree".into()) },
            "all_lines": o.all_lines,
            "files": files.len(),
            "changed_lines": if o.all_lines { None } else { Some(changed_lines) },
            "git_safe": o.git_safe,
            "merge_base": o.merge_base,
        }),
        &unmatched,
    );
    change.files = files;
    Ok(change)
}

/// `describe` plus `"unmatched": [..]` (only when some `--files` glob matched nothing).
fn with_unmatched(mut describe: serde_json::Value, unmatched: &[String]) -> serde_json::Value {
    if !unmatched.is_empty()
        && let Some(map) = describe.as_object_mut()
    {
        map.insert("unmatched".into(), json!(unmatched));
    }
    describe
}

/// `modern-tara.toml` as the scanned source holds it: `None` when it is not
/// there, an error when it is listed but cannot be read (never a silent "no
/// file").
fn read_repo_file(change: &Change) -> Result<Option<String>, String> {
    if !change.exists(waiver::FILE) {
        return Ok(None);
    }
    let cannot = |why: String| format!("{}: cannot read: {why}", waiver::FILE);
    let mut found = change.read(&[waiver::FILE.to_string()]).map_err(cannot)?;
    let bytes = found.remove(waiver::FILE).ok_or_else(|| cannot(format!("unreadable or over {} bytes", change::MAX_FILE_BYTES)))?;
    String::from_utf8(bytes).map(Some).map_err(|_| cannot("not UTF-8".into()))
}

/// The repository file's two parts. `[js]` always comes from the scanned
/// source's `modern-tara.toml`. The waivers: `--no-waivers` none (the file's
/// `[[waiver]]` list is not even read), `--waivers FILE` that file from disk
/// (the repository's own list is ignored), else the repository file's.
fn load_config(o: &cli::Opts, change: &Change) -> Result<waiver::RepoFile, String> {
    let own_waivers = if let Some(path) = &o.waivers {
        let name = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| format!("--waivers {name}: {e}"))?;
        Some(waiver::Set::load(&name, &text)?)
    } else {
        None
    };
    let mut config = match read_repo_file(change)? {
        Some(text) => waiver::RepoFile::load(&text, !o.no_waivers && own_waivers.is_none())?,
        None => waiver::RepoFile::default(),
    };
    if let Some(set) = own_waivers {
        config.waivers = set;
    }
    Ok(config)
}

/// The warning for each `--files` glob that matched nothing.
fn unmatched_warnings(change: &Change) -> Vec<String> {
    let list = change.describe.get("unmatched").and_then(serde_json::Value::as_array);
    list.into_iter().flatten().filter_map(serde_json::Value::as_str).map(|p| format!("--files {p} matches nothing")).collect()
}

/// `--files` patterns resolved against the source's listing, relative to the
/// `--repo` folder (`prefix` below the top level). Per pattern: first the
/// literal path (so `app/[id]/page.tsx` and a `--repo` folder named `[id]`
/// are never read as globs), else, when the pattern itself has glob
/// characters, the glob over the listed files under `prefix`, else an error.
/// Returns the files and the globs (as given) that matched nothing: those are
/// warnings, an error only when no pattern matched anything.
fn select(change: &Change, patterns: &[String], prefix: &str, dir: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    let listing = change.listing()?;
    let dir_text = dir.to_string_lossy();
    let mut out = Vec::new();
    let mut unmatched = Vec::new();
    let literal = |n: &str| -> Option<String> {
        match listing.iter().find(|l| l.eq_ignore_ascii_case(n)) {
            Some(l) => Some(l.clone()),
            None if matches!(change.source, Source::WorkTree(_) | Source::Dir) && change.root.join(n).is_file() => Some(n.to_string()),
            None => None,
        }
    };
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
        let normalized = change::normalize(&full);
        if let Some(found) = normalized.as_deref().and_then(literal) {
            out.push(found);
        } else if change::is_glob(p) {
            let glob = change::Glob::new(p).map_err(|e| format!("--files {raw}: {e}"))?;
            let before = out.len();
            for entry in listing {
                let inside = if prefix.is_empty() { Some(entry.clone()) } else { change::strip_dir(entry, prefix) };
                if inside.is_some_and(|rest| glob.matches(&rest)) {
                    out.push(entry.clone());
                }
            }
            if out.len() == before {
                unmatched.push(raw.clone());
            }
        } else if normalized.is_none() {
            return Err(format!("--files {raw}: outside the repository"));
        } else {
            return Err(format!("--files {raw}: no such file"));
        }
    }
    if !patterns.is_empty() && unmatched.len() == patterns.len() {
        return Err(format!("--files matched nothing: {}", unmatched.join(", ")));
    }
    out.sort();
    out.dedup();
    Ok((out, unmatched))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain folder with these files, scanned as `Source::Dir`.
    fn folder(name: &str, files: &[&str]) -> (PathBuf, Change) {
        let root = std::env::temp_dir().join(format!("mt-select-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for f in files {
            let path = root.join(f);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        let change = Change::new(root.clone(), Source::Dir, BTreeMap::new(), json!({}));
        (root, change)
    }

    fn pats(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn select_collects_unmatched_globs_and_goes_on() {
        let (root, change) = folder("some", &["a.py", "b.py", "sub/c.py"]);
        let (files, unmatched) = select(&change, &pats(&["[a-b].py", "*.rs", "{sub,lib}/*.py", "nope/**"]), "", &root).unwrap();
        assert_eq!(files, vec!["a.py", "b.py", "sub/c.py"]);
        assert_eq!(unmatched, vec!["*.rs", "nope/**"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn select_errors_only_when_every_pattern_matches_nothing() {
        let (root, change) = folder("none", &["a.py"]);
        let err = select(&change, &pats(&["*.rs", "x/[0-9].py"]), "", &root).unwrap_err();
        assert_eq!(err, "--files matched nothing: *.rs, x/[0-9].py");
        // A plain path that is missing stays an error of its own, even next to a match.
        let err = select(&change, &pats(&["*.py", "missing.py"]), "", &root).unwrap_err();
        assert_eq!(err, "--files missing.py: no such file");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn select_takes_an_existing_literal_path_before_any_glob() {
        // `[id]` as a class would match the folder `d`: the literal file wins.
        let (root, change) = folder("literal", &["app/[id]/page.css", "app/d/page.css", "app/i/other.tsx"]);
        let (files, unmatched) = select(&change, &pats(&["app/[id]/page.css"]), "", &root).unwrap();
        assert_eq!(files, vec!["app/[id]/page.css"]);
        assert_eq!(unmatched, Vec::<String>::new());
        // No such literal path: the class is a class.
        let (files, _) = select(&change, &pats(&["app/[id]/*.tsx"]), "", &root).unwrap();
        assert_eq!(files, vec!["app/i/other.tsx"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn select_does_not_read_the_repo_prefix_as_a_glob() {
        let (root, change) = folder("prefix", &["app/[id]/page.css", "app/d/page.css", "app/{x,y}/a.css", "app/x/b.css", "app/y/c.css"]);
        // `--repo app/[id] --files page.css`: only the file inside --repo.
        let (files, _) = select(&change, &pats(&["page.css"]), "app/[id]", &root).unwrap();
        assert_eq!(files, vec!["app/[id]/page.css"]);
        // `--repo "app/{x,y}" --files "*.css"`: the glob runs below the folder, not on `prefix/*.css`.
        let (files, unmatched) = select(&change, &pats(&["*.css"]), "app/{x,y}", &root).unwrap();
        assert_eq!(files, vec!["app/{x,y}/a.css"]);
        assert_eq!(unmatched, Vec::<String>::new());
        let (files, _) = select(&change, &pats(&["**/*.css"]), "app/[id]", &root).unwrap();
        assert_eq!(files, vec!["app/[id]/page.css"]);
        // A folder with no match for the glob: unmatched, and an error when alone.
        let err = select(&change, &pats(&["*.tsx"]), "app/[id]", &root).unwrap_err();
        assert_eq!(err, "--files matched nothing: *.tsx");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn select_globs_below_a_plain_prefix() {
        let (root, change) = folder("below", &["sub/a.py", "sub/deep/b.py", "SUB2/c.py", "top.py"]);
        let (files, _) = select(&change, &pats(&["*.py"]), "sub", &root).unwrap();
        assert_eq!(files, vec!["sub/a.py"]);
        let (files, _) = select(&change, &pats(&["**/*.py"]), "SUB", &root).unwrap();
        assert_eq!(files, vec!["sub/a.py", "sub/deep/b.py"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_repository_file_must_be_readable() {
        let o = cli::Opts::default();
        let (root, change) = folder("waivers", &["a.py"]);
        assert!(load_config(&o, &change).unwrap().waivers.file.is_none(), "no file: no waivers");
        let write = |bytes: &[u8]| std::fs::write(root.join(waiver::FILE), bytes).unwrap();
        // The listing is read once, so each case gets a fresh Change.
        let fresh = || Change::new(root.clone(), Source::Dir, BTreeMap::new(), json!({}));
        write(b"[[waiver]]\nrule = \"R\"\npaths = [\"*.py\"]\nreason = \"r\"\n");
        let set = load_config(&o, &fresh()).unwrap().waivers;
        assert_eq!((set.file.as_deref(), set.entries.len()), (Some("modern-tara.toml"), 1));
        write(&[0xff, 0xfe, b'x']);
        let err = load_config(&o, &fresh()).map(|_| ()).unwrap_err();
        assert!(err.starts_with("modern-tara.toml: cannot read: ") && err.contains("UTF-8"), "{err}");
        write(&vec![b'#'; change::MAX_FILE_BYTES + 1]);
        let err = load_config(&o, &fresh()).map(|_| ()).unwrap_err();
        assert!(err.starts_with("modern-tara.toml: cannot read: "), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn classic_scripts_survive_no_waivers_and_waivers_file() {
        let (root, _) = folder("classic", &["a.py"]);
        let fresh = || Change::new(root.clone(), Source::Dir, BTreeMap::new(), json!({}));
        // The repository's waiver entry is broken: only `[js]` may be read when the list is off or replaced.
        let repo = "[js]\nclassic-scripts = [\"public/**/*.js\"]\n\n[[waiver]]\nrule = \"R\"\nbogus = 1\n";
        std::fs::write(root.join(waiver::FILE), repo).unwrap();
        let off = cli::Opts { no_waivers: true, ..cli::Opts::default() };
        let config = load_config(&off, &fresh()).unwrap();
        assert!(config.waivers.file.is_none() && config.waivers.entries.is_empty());
        assert!(config.classic_scripts.iter().any(|g| g.matches("public/a/app.js")), "--no-waivers keeps classic-scripts");
        let err = load_config(&cli::Opts::default(), &fresh()).map(|_| ()).unwrap_err();
        assert!(err.contains("waiver 1: unknown key `bogus`"), "{err}");

        let other = root.join("other.toml");
        std::fs::write(&other, "[[waiver]]\nrule = \"R2\"\npaths = [\"*.py\"]\nreason = \"r\"\n").unwrap();
        let with_file = cli::Opts { waivers: Some(other.clone()), ..cli::Opts::default() };
        let config = load_config(&with_file, &fresh()).unwrap();
        assert_eq!(config.waivers.entries.len(), 1);
        assert_eq!(config.waivers.entries[0].rule, "R2", "the repository's own list is ignored");
        assert_eq!(config.waivers.file, Some(other.display().to_string()));
        assert!(config.classic_scripts.iter().any(|g| g.matches("public/app.js")), "--waivers keeps the repository's [js]");

        std::fs::write(&other, "[js]\nclassic-scripts = [\"x/*.js\"]\n").unwrap();
        let err = load_config(&with_file, &fresh()).map(|_| ()).unwrap_err();
        assert_eq!(err, format!("{}: [js] belongs in the repository's modern-tara.toml", other.display()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn select_names_a_pattern_over_the_brace_cap() {
        let (root, change) = folder("cap", &["a.py"]);
        let over = "{a,b}".repeat(9);
        let err = select(&change, &pats(&["a.py", &over]), "", &root).unwrap_err();
        assert!(err.starts_with(&format!("--files {over}: ")) && err.contains("256"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unmatched_lands_in_describe_and_warnings() {
        let described = with_unmatched(json!({"mode": "files"}), &pats(&["*.rs"]));
        assert_eq!(described["unmatched"], json!(["*.rs"]));
        assert!(with_unmatched(json!({"mode": "files"}), &[]).get("unmatched").is_none());
        let change = Change::new(PathBuf::new(), Source::Dir, BTreeMap::new(), described);
        assert_eq!(unmatched_warnings(&change), vec!["--files *.rs matches nothing"]);
    }
}
