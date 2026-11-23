//! The composite action's steps, run the way a GitHub runner runs them: the
//! `run:` block of a step is cut out of `action.yml` by plain text parsing,
//! saved as a script and started with `pwsh -command ". '<script>'"`, with the
//! step's `env:` names set by hand. Repositories are built at test time; a
//! shallow clone stands in for `actions/checkout` with its default depth. The
//! download step runs only as far as its version check (a dead proxy keeps
//! it off the network). A machine without `pwsh` or `git` skips every test.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

const ZEROS: &str = "0000000000000000000000000000000000000000";

fn available() -> bool {
    let ok = |program: &str, args: &[&str]| Command::new(program).args(args).output().is_ok_and(|o| o.status.success());
    let have = ok("pwsh", &["-NoProfile", "-NonInteractive", "-Command", "exit 0"]) && ok("git", &["--version"]);
    if !have {
        eprintln!("skipped: pwsh and git are both needed");
    }
    have
}

/// The lines of `action.yml` between the step called `name` and the next one.
fn step_lines(name: &str) -> Vec<String> {
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("action.yml")).unwrap();
    let header = format!("    - name: {name}");
    let lines = text.lines().skip_while(|l| l.trim_end() != header).skip(1).take_while(|l| !l.starts_with("    - ")).map(str::to_string).collect::<Vec<_>>();
    assert!(!lines.is_empty(), "no step called {name} in action.yml");
    lines
}

/// The step's `run: |` block with its indentation removed.
fn run_block(name: &str) -> String {
    let lines = step_lines(name);
    let at = lines.iter().position(|l| l.trim_end() == "      run: |").unwrap_or_else(|| panic!("step {name} has no `run: |` block"));
    let mut out = String::new();
    for line in &lines[at + 1..] {
        if line.trim().is_empty() {
            out.push('\n');
        } else if let Some(body) = line.strip_prefix("        ") {
            out.push_str(body);
            out.push('\n');
        } else {
            break;
        }
    }
    out
}

/// The names under the step's `env:`.
fn env_names(name: &str) -> Vec<String> {
    let lines = step_lines(name);
    let at = lines.iter().position(|l| l.trim_end() == "      env:").unwrap_or_else(|| panic!("step {name} has no env block"));
    lines[at + 1..].iter().take_while(|l| l.starts_with("        ")).filter_map(|l| l.trim().split_once(':').map(|(k, _)| k.to_string())).collect()
}

struct Work(PathBuf);

impl Work {
    fn new() -> Work {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mt-act-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("runner")).unwrap();
        Work(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git_cmd(dir: &Path, args: &[&str]) -> std::process::Output {
    let hooks = dir.join(".no-hooks");
    Command::new("git")
        .args(["-c", "user.name=modern-tara-test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_cmd(dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn url(dir: &Path) -> String {
    let p = dir.display().to_string().replace('\\', "/");
    if p.starts_with('/') { format!("file://{p}") } else { format!("file:///{p}") }
}

fn init(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
}

/// A commit that appends a line to `notes.md`; its id.
fn commit(dir: &Path, line: &str) -> String {
    let file = dir.join("notes.md");
    let mut text = fs::read_to_string(&file).unwrap_or_default();
    text.push_str(line);
    text.push('\n');
    fs::write(&file, text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", line]);
    git(dir, &["rev-parse", "HEAD"])
}

/// What `actions/checkout` leaves: a clone with `depth` 1.
fn shallow_clone(work: &Work, origin: &Path, extra: &[&str]) -> PathBuf {
    let dest = work.path("clone");
    let mut args = vec!["clone", "-q", "--depth", "1"];
    args.extend_from_slice(extra);
    let src = url(origin);
    args.push(&src);
    args.push("clone");
    git(&work.0, &args);
    assert_eq!(git(&dest, &["rev-parse", "--is-shallow-repository"]), "true");
    dest
}

/// `main`: A - B; `feature`: A - C. Returns (A, B, C), `main` checked out.
fn forked(origin: &Path) -> (String, String, String) {
    init(origin);
    let a = commit(origin, "a");
    git(origin, &["checkout", "-q", "-b", "feature"]);
    let c = commit(origin, "c");
    git(origin, &["checkout", "-q", "main"]);
    let b = commit(origin, "b");
    (a, b, c)
}

struct Run {
    code: i32,
    out: String,
    report: Option<Value>,
}

/// Runs the `run:` block of `step` in `repo` under the form GitHub uses.
fn run_step(work: &Work, step: &str, repo: &Path, env: &[(&str, &str)]) -> Run {
    let script = work.path("step.ps1");
    fs::write(&script, run_block(step)).unwrap();
    let runner = work.path("runner");
    let report = runner.join("modern-tara.json");
    let _ = fs::remove_file(&report);
    let summary = runner.join("job-summary.md");
    let output = runner.join("output.txt");
    let sha = git_cmd(repo, &["rev-parse", "HEAD"]);
    let mut head = String::from_utf8_lossy(&sha.stdout).trim().to_string();
    if head.is_empty() {
        head = "1111111111111111111111111111111111111111".into();
    }
    let bin = env!("CARGO_BIN_EXE_modern-tara");
    let mut vars: Vec<(&str, String)> = vec![
        ("GITHUB_EVENT_NAME", "push".into()),
        ("GITHUB_SHA", head),
        ("RUNNER_TEMP", runner.display().to_string()),
        ("GITHUB_STEP_SUMMARY", summary.display().to_string()),
        ("GITHUB_OUTPUT", output.display().to_string()),
        ("MT_EXE", bin.into()),
        ("MODERN_TARA_HOME", env!("CARGO_MANIFEST_DIR").into()),
        ("MODERN_TARA_CACHE", work.path("cache").display().to_string()),
        ("MT_RANGE", String::new()),
        ("MT_ARGS", "--stacks python".into()),
        ("MT_WAIVERS", String::new()),
        ("MT_ALLOW_SKIPPED", String::new()),
        ("MT_PR_BASE", String::new()),
        ("MT_PR_HEAD", String::new()),
        ("MT_BEFORE", String::new()),
        ("MT_VERSION", String::new()),
        ("MT_ACTION_REF", String::new()),
        ("MT_ACTION_REPO", String::new()),
        ("GH_TOKEN", String::new()),
        ("HTTP_PROXY", "http://127.0.0.1:9".into()),
        ("HTTPS_PROXY", "http://127.0.0.1:9".into()),
    ];
    for (k, v) in env {
        vars.retain(|(name, _)| name != k);
        vars.push((k, (*v).to_string()));
    }
    // A new input in the step must get a value here, or the test would run it empty.
    for name in env_names(step) {
        assert!(vars.iter().any(|(k, _)| *k == name) || name == "GH_TOKEN", "step {step} reads {name}: give it a value in tests/action.rs");
    }
    let mut cmd = Command::new("pwsh");
    cmd.args(["-NoProfile", "-NonInteractive", "-command"]).arg(format!(". '{}'", script.display())).current_dir(repo);
    for (k, v) in &vars {
        cmd.env(k, v);
    }
    cmd.env("PATH", prepend_path(Path::new(bin).parent().unwrap()));
    let out = cmd.output().expect("pwsh");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let report = fs::read(&report).ok().and_then(|b| serde_json::from_slice(&b).ok());
    Run { code: out.status.code().unwrap_or(-1), out: flatten(&text), report }
}

/// pwsh prints a thrown message as a block that it word-wraps to the console
/// width, each line after a `     | ` marker, with colour codes around. This
/// drops the codes, the source excerpt and the `~~~` marker and joins the
/// rest with single spaces, so an assertion does not depend on the width.
fn flatten(text: &str) -> String {
    let mut plain = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                for e in chars.by_ref() {
                    if e.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            '\u{7}' | '\r' => {}
            _ => plain.push(c),
        }
    }
    let mut words: Vec<&str> = Vec::new();
    for line in plain.lines() {
        let body = match line.split_once('|') {
            Some((before, after)) if before.trim().is_empty() || before.trim() == "Line" => after,
            Some((before, _)) if before.trim().chars().all(|c| c.is_ascii_digit()) => continue,
            _ => line,
        };
        if body.trim().chars().all(|c| c == '~') {
            continue;
        }
        words.extend(body.split_whitespace());
    }
    words.join(" ")
}

#[test]
fn wrapped_error_text_is_joined_whatever_the_console_width() {
    // Captured from pwsh 7.6 for a `throw`, wrapped at 120 columns, and the
    // same text re-wrapped at 40.
    let wide = "\u{1b}[31;1mException: \u{1b}[0mC:\\t\\step.ps1:1\u{1b}[0m\r\n\u{1b}[36;1mLine |\u{1b}[0m\r\n\u{1b}[36;1m   1 | \u{1b}[0m \u{1b}[36;1mthrow \"modern-tara: version is older\u{1b}[0m \u{7}\u{1b}[0m\r\n\u{1b}[36;1m     | \u{1b}[31;1m ~~~~~~~~~~~~~~~~~~~~~~~~\u{1b}[0m\r\n\u{1b}[36;1m     | \u{1b}[31;1mmodern-tara: version is older than this action, which needs modern-tara 0.5.0 or newer (older releases reject\u{1b}[0m\r\n\u{1b}[36;1m     | \u{1b}[31;1m--range a...b and --allow-skipped); use v0.5.0 or later\u{1b}[0m\r\n";
    let narrow = "Exception: C:\\t\\step.ps1:1\nLine |\n   1 |  throw \"modern-tara: ver\n     |  ~~~~~~~~~~~~~~~~~~~~~~~~\n     | modern-tara: version is older than this\n     | action, which needs modern-tara 0.5.0 or\n     | newer (older releases reject --range\n     | a...b and --allow-skipped); use v0.5.0\n     | or later\n";
    for text in [wide, narrow] {
        let flat = flatten(text);
        assert!(
            flat.contains("which needs modern-tara 0.5.0 or newer (older releases reject --range a...b and --allow-skipped); use v0.5.0 or later"),
            "{flat}"
        );
        assert!(!flat.contains('|'), "{flat}");
    }
}

fn prepend_path(dir: &Path) -> std::ffi::OsString {
    let mut parts = vec![dir.to_path_buf()];
    parts.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    std::env::join_paths(parts).unwrap()
}

fn change(run: &Run) -> &Value {
    let report = run.report.as_ref().unwrap_or_else(|| panic!("no JSON report (exit {}):\n{}", run.code, run.out));
    &report["change"]
}

#[test]
fn steps_are_found() {
    assert!(run_block("Scan").contains("$host.SetShouldExit($code)"));
    assert!(env_names("Scan").contains(&"MT_RANGE".to_string()));
    assert!(run_block("Download and verify modern-tara").contains("-lt [version]'0.5.0'"));
}

#[test]
fn pull_request_on_a_depth_one_clone_reaches_the_merge_base() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    let (a, b, c) = forked(&origin);
    let clone = shallow_clone(&work, &origin, &["--branch", "feature"]);
    let run = run_step(&work, "Scan", &clone, &[("GITHUB_EVENT_NAME", "pull_request"), ("MT_PR_BASE", &b), ("MT_PR_HEAD", &c)]);
    assert_eq!(run.code, 0, "{}", run.out);
    let ch = change(&run);
    assert_eq!(ch["merge_base"], true, "{}", run.out);
    assert_eq!(ch["from"], a.as_str());
    assert_eq!(ch["to"], c.as_str());
}

#[test]
fn push_with_an_all_zero_before_scans_the_last_commit() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    init(&origin);
    commit(&origin, "a");
    let b = commit(&origin, "b");
    let c = commit(&origin, "c");
    let clone = shallow_clone(&work, &origin, &[]);
    let run = run_step(&work, "Scan", &clone, &[("MT_BEFORE", ZEROS)]);
    assert_eq!(run.code, 0, "{}", run.out);
    assert_eq!(change(&run)["from"], b.as_str());
    assert_eq!(change(&run)["to"], c.as_str());
}

#[test]
fn push_scans_from_before() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    init(&origin);
    let a = commit(&origin, "a");
    commit(&origin, "b");
    let c = commit(&origin, "c");
    let clone = shallow_clone(&work, &origin, &[]);
    let run = run_step(&work, "Scan", &clone, &[("MT_BEFORE", &a)]);
    assert_eq!(run.code, 0, "{}", run.out);
    assert_eq!(change(&run)["from"], a.as_str());
    assert_eq!(change(&run)["to"], c.as_str());
}

#[test]
fn push_of_a_root_commit_scans_every_line() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    init(&origin);
    commit(&origin, "a");
    let clone = shallow_clone(&work, &origin, &[]);
    let run = run_step(&work, "Scan", &clone, &[("MT_BEFORE", ZEROS)]);
    assert_eq!(run.code, 0, "{}", run.out);
    assert_eq!(change(&run)["all_lines"], true, "{}", run.out);
}

#[test]
fn explicit_three_dot_range_on_a_shallow_clone_finds_the_merge_base() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    let (a, _, c) = forked(&origin);
    let clone = shallow_clone(&work, &origin, &["--no-single-branch", "--branch", "feature"]);
    let run = run_step(&work, "Scan", &clone, &[("MT_RANGE", "origin/main...HEAD")]);
    assert_eq!(run.code, 0, "{}", run.out);
    assert_eq!(change(&run)["merge_base"], true);
    assert_eq!(change(&run)["from"], a.as_str());
    assert_eq!(change(&run)["to"], c.as_str());
}

#[test]
fn explicit_two_dot_range_is_used_as_given() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    init(&origin);
    let a = commit(&origin, "a");
    let b = commit(&origin, "b");
    let clone = work.path("clone");
    git(&work.0, &["clone", "-q", &url(&origin), "clone"]);
    let run = run_step(&work, "Scan", &clone, &[("MT_RANGE", &format!("{a}..{b}"))]);
    assert_eq!(run.code, 0, "{}", run.out);
    assert_eq!(change(&run)["merge_base"], false);
}

#[test]
fn failed_history_fetch_names_the_fix() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    init(&origin);
    commit(&origin, "a");
    commit(&origin, "b");
    let clone = shallow_clone(&work, &origin, &[]);
    git(&clone, &["remote", "set-url", "origin", &url(&work.path("gone"))]);
    for env in [vec![], vec![("MT_RANGE", "HEAD~1...HEAD")]] {
        let run = run_step(&work, "Scan", &clone, &env);
        assert_ne!(run.code, 0, "{}", run.out);
        assert!(run.out.contains("could not fetch the full history from origin"), "{}", run.out);
        assert!(run.out.contains("fetch-depth: 0"), "{}", run.out);
    }
}

#[test]
fn failed_pull_request_fetch_names_the_fix() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    let (_, b, c) = forked(&origin);
    let clone = work.path("clone");
    // The pull request head is not in the clone, and origin cannot be reached.
    git(&work.0, &["clone", "-q", "--single-branch", "--branch", "main", &url(&origin), "clone"]);
    git(&clone, &["remote", "set-url", "origin", &url(&work.path("gone"))]);
    let run = run_step(&work, "Scan", &clone, &[("GITHUB_EVENT_NAME", "pull_request"), ("MT_PR_BASE", &b), ("MT_PR_HEAD", &c)]);
    assert_ne!(run.code, 0, "{}", run.out);
    assert!(run.out.contains("could not fetch the pull request commits"), "{}", run.out);
    assert!(run.out.contains("fetch-depth: 0"), "{}", run.out);
}

#[test]
fn version_older_than_the_action_is_refused() {
    if !available() {
        return;
    }
    for old in ["v0.4.0", "v0.4.9", "v0.0.1"] {
        let work = Work::new();
        let repo = work.path("repo");
        init(&repo);
        commit(&repo, "a");
        let run = run_step(&work, "Download and verify modern-tara", &repo, &[("MT_VERSION", old)]);
        assert_ne!(run.code, 0, "{old}: {}", run.out);
        assert!(run.out.contains("needs modern-tara 0.5.0 or newer"), "{old}: {}", run.out);
        assert!(run.out.contains("--allow-skipped"), "{old}: {}", run.out);
    }
}

#[test]
fn version_from_the_action_ref_is_checked_too() {
    if !available() {
        return;
    }
    let work = Work::new();
    let repo = work.path("repo");
    init(&repo);
    commit(&repo, "a");
    let run = run_step(&work, "Download and verify modern-tara", &repo, &[("MT_ACTION_REF", "v0.3.2")]);
    assert_ne!(run.code, 0, "{}", run.out);
    assert!(run.out.contains("needs modern-tara 0.5.0 or newer"), "{}", run.out);
}

#[test]
fn version_from_0_5_0_passes_the_check() {
    if !available() {
        return;
    }
    // v0.10.0 is newer than v0.5.0 as a number; the step then fails on the
    // dead proxy, not on the version.
    for ok in ["v0.5.0", "v0.10.0", "v1.0.0"] {
        let work = Work::new();
        let repo = work.path("repo");
        init(&repo);
        commit(&repo, "a");
        let run = run_step(&work, "Download and verify modern-tara", &repo, &[("MT_VERSION", ok)]);
        assert_ne!(run.code, 0, "{ok}: the download cannot work here: {}", run.out);
        assert!(!run.out.contains("0.5.0 or newer"), "{ok}: {}", run.out);
    }
}

/// A stand-in for `modern-tara` that ends with `code`.
fn stub(work: &Work, code: i32) -> PathBuf {
    if cfg!(windows) {
        let path = work.path(format!("stub{code}.cmd").as_str());
        fs::write(&path, format!("@echo modern-tara: stub summary 1>&2\r\n@exit /b {code}\r\n")).unwrap();
        path
    } else {
        let path = work.path(format!("stub{code}.sh").as_str());
        fs::write(&path, format!("#!/bin/sh\necho 'modern-tara: stub summary' >&2\nexit {code}\n")).unwrap();
        let chmod = Command::new("chmod").arg("0755").arg(&path).status().expect("chmod");
        assert!(chmod.success());
        path
    }
}

#[test]
fn exit_code_of_the_scan_reaches_the_job() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    init(&origin);
    commit(&origin, "a");
    for code in [0, 1, 2, 3] {
        let exe = stub(&work, code);
        let run = run_step(&work, "Scan", &origin, &[("MT_EXE", &exe.display().to_string())]);
        assert_eq!(run.code, code, "{}", run.out);
        assert!(run.out.contains("stub summary"), "{}", run.out);
        let summary = fs::read_to_string(work.path("runner").join("job-summary.md")).unwrap();
        assert!(summary.contains("stub summary"), "{summary}");
    }
}

#[test]
fn a_failed_scan_is_exit_2() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    init(&origin);
    commit(&origin, "a");
    commit(&origin, "b");
    let run = run_step(&work, "Scan", &origin, &[("MT_RANGE", "no-such-commit..HEAD")]);
    assert_eq!(run.code, 2, "{}", run.out);
    assert!(run.out.contains("not a commit in this clone: no-such-commit (shallow clone? use fetch-depth: 0)"), "{}", run.out);
}

#[test]
fn a_job_without_a_checkout_fails_instead_of_scanning_nothing() {
    if !available() {
        return;
    }
    let work = Work::new();
    // No repository at all: the folder is empty and git fails everywhere.
    let empty = work.path("empty");
    fs::create_dir_all(&empty).unwrap();
    let run = run_step(&work, "Scan", &empty, &[("MT_BEFORE", ZEROS)]);
    assert_eq!(run.code, 2, "{}", run.out);
    assert!(run.out.contains("run actions/checkout before this action"), "{}", run.out);
    assert!(run.report.is_none(), "{}", run.out);
    // A repository that does not hold GITHUB_SHA (another repository was checked out, or `path:` was set).
    let origin = work.path("origin");
    init(&origin);
    commit(&origin, "a");
    let run = run_step(&work, "Scan", &origin, &[("GITHUB_SHA", "2222222222222222222222222222222222222222"), ("MT_BEFORE", ZEROS)]);
    assert_eq!(run.code, 2, "{}", run.out);
    assert!(run.out.contains("run actions/checkout before this action"), "{}", run.out);
}

/// A stand-in for `modern-tara` that writes its arguments, space-separated, to `args.txt` next to it.
fn echo_stub(work: &Work) -> PathBuf {
    if cfg!(windows) {
        let path = work.path("echo.cmd");
        fs::write(&path, "@echo %* > \"%~dp0args.txt\"\r\n@exit /b 0\r\n").unwrap();
        path
    } else {
        let path = work.path("echo.sh");
        fs::write(&path, "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args.txt\"\nexit 0\n").unwrap();
        assert!(Command::new("chmod").arg("0755").arg(&path).status().expect("chmod").success());
        path
    }
}

#[test]
fn inputs_reach_the_command_line_and_the_report_output_is_set() {
    if !available() {
        return;
    }
    let work = Work::new();
    let origin = work.path("origin");
    init(&origin);
    commit(&origin, "a");
    commit(&origin, "b");
    fs::write(origin.join("waivers.toml"), "").unwrap();
    let exe = echo_stub(&work);
    let exe = exe.display().to_string();
    let args = |run: &Run| -> Vec<String> {
        assert_eq!(run.code, 0, "{}", run.out);
        fs::read_to_string(work.path("args.txt")).unwrap().split_whitespace().map(str::to_string).collect()
    };
    let has = |args: &[String], pair: &[&str]| args.windows(pair.len()).any(|w| w.iter().map(String::as_str).eq(pair.iter().copied()));

    let run = run_step(
        &work,
        "Scan",
        &origin,
        &[
            ("MT_EXE", &exe),
            ("MT_BEFORE", ZEROS),
            ("MT_ARGS", "--stacks rust,python  --quiet"),
            ("MT_WAIVERS", "waivers.toml"),
            ("MT_ALLOW_SKIPPED", "js,rust"),
        ],
    );
    let got = args(&run);
    let report = work.path("runner").join("modern-tara.json").display().to_string();
    assert_eq!(&got[..3], ["scan", "--repo", "."], "{got:?}");
    assert!(has(&got, &["--stacks", "rust,python", "--quiet"]), "{got:?}");
    assert!(has(&got, &["--waivers", "waivers.toml"]), "{got:?}");
    assert!(has(&got, &["--allow-skipped", "js,rust"]), "{got:?}");
    assert!(has(&got, &["--json", &report]), "{got:?}");
    assert!(got.iter().any(|a| a == "--gate"), "{got:?}");
    let output = fs::read_to_string(work.path("runner").join("output.txt")).unwrap();
    assert!(output.lines().any(|l| l == format!("report={report}")), "{output}");

    // Empty inputs add nothing.
    let run = run_step(&work, "Scan", &origin, &[("MT_EXE", &exe), ("MT_BEFORE", ZEROS), ("MT_ARGS", "")]);
    let got = args(&run);
    assert!(!got.iter().any(|a| a == "--waivers" || a == "--allow-skipped"), "{got:?}");

    // A waivers file that is not there stops the step.
    let run = run_step(&work, "Scan", &origin, &[("MT_EXE", &exe), ("MT_BEFORE", ZEROS), ("MT_WAIVERS", "missing.toml")]);
    assert_ne!(run.code, 0, "{}", run.out);
    assert!(run.out.contains("waivers file 'missing.toml' not found"), "{}", run.out);
}
