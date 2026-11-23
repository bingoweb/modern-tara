//! End-to-end runs on tiny repositories built at test time from
//! `tests/fixtures/<stack>/{base,change}`: `base` is the first commit,
//! `change` is overlaid as the second. Marker comments in `change` say what
//! must be found: `OLD:<rule part>:<kind>` on a line needs such a finding,
//! `DECOY` and `UNCHANGED` lines must have none. A stack whose tool is not
//! installed reports `skipped` and the test passes with a note.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

/// The Rust stack's target-dir cache: a sibling of the test repository,
/// removed with it (never inside it: the repo must stay clean).
fn cache_for(repo: &Path) -> PathBuf {
    PathBuf::from(format!("{}-cache", repo.display()))
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

struct TempRepo(PathBuf);

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        let _ = fs::remove_dir_all(cache_for(&self.0));
    }
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let hooks = dir.join(".no-hooks");
    let out = Command::new("git")
        .args(["-c", "user.name=modern-tara-test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn repo(stack: &str) -> TempRepo {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mt-test-{stack}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let guard = TempRepo(dir.clone());
    copy_tree(&fixtures().join(stack).join("base"), &dir);
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "--no-verify", "-m", "base"]);
    copy_tree(&fixtures().join(stack).join("change"), &dir);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "--no-verify", "-m", "change"]);
    guard
}

fn scan(dir: &Path, extra: &[&str]) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_modern-tara"))
        .arg("scan")
        .arg("--repo")
        .arg(dir)
        .args(extra)
        .arg("--quiet")
        .env("MODERN_TARA_CACHE", cache_for(dir))
        .env("MODERN_TARA_HOME", env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("modern-tara");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("bad JSON ({e}): {}", String::from_utf8_lossy(&out.stderr)));
    (out.status.code().unwrap_or(-1), v)
}

/// `name` on PATH, else where the install scripts or cargo put it.
fn installed(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let home = std::env::home_dir().unwrap_or_default();
    let path: Vec<PathBuf> = std::env::var_os("PATH").map_or_default(|p| std::env::split_paths(&p).collect());
    path.into_iter().chain([home.join(".local").join("bin"), home.join(".cargo").join("bin")]).map(|d| d.join(&file)).find(|p| p.is_file())
}

/// Like `scan`, for runs that may fail before a report exists: the exit
/// code, stdout and stderr as text (no `--quiet`, so stderr has the summary).
fn scan_raw(dir: &Path, extra: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_modern-tara"))
        .arg("scan")
        .arg("--repo")
        .arg(dir)
        .args(extra)
        .env("MODERN_TARA_CACHE", cache_for(dir))
        .env("MODERN_TARA_HOME", env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("modern-tara");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A one-commit Python repository: `pyproject.toml` with `requires` and `app.py`.
fn python_repo(name: &str, requires: &str, code: &str) -> TempRepo {
    python_repo_with(name, requires, code, &[])
}

/// `python_repo` plus `extra` (path, content) files in the same commit.
fn python_repo_with(name: &str, requires: &str, code: &str, extra: &[(&str, &str)]) -> TempRepo {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mt-test-{name}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let guard = TempRepo(dir.clone());
    fs::write(dir.join("pyproject.toml"), format!("[project]\nname = \"gate\"\nversion = \"0\"\nrequires-python = \"{requires}\"\n")).unwrap();
    fs::write(dir.join("app.py"), code).unwrap();
    for (path, content) in extra {
        let target = dir.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, content).unwrap();
    }
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "--no-verify", "-m", "one"]);
    guard
}

/// `(file, line, marker)` for every marker in the `change` tree.
fn markers(stack: &str) -> Vec<(String, u32, String)> {
    let root = fixtures().join(stack).join("change");
    let mut out = Vec::new();
    let mut stack_dirs = vec![root.clone()];
    while let Some(d) = stack_dirs.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                stack_dirs.push(e.path());
                continue;
            }
            let rel = e.path().strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
            for (i, line) in fs::read_to_string(e.path()).unwrap().lines().enumerate() {
                let n = u32::try_from(i + 1).unwrap();
                if let Some(at) = line.find("OLD:") {
                    let m = line[at + 4..].split([' ', '*']).next().unwrap().to_string();
                    out.push((rel.clone(), n, m));
                } else if line.contains("DECOY") || line.contains("UNCHANGED") {
                    out.push((rel.clone(), n, String::new()));
                }
            }
        }
    }
    out
}

/// Runs the fixture of `stack` and checks every marker.
fn check(stack: &str) {
    check_fixture(stack, stack);
}

/// Like `check`, for fixture folder `fixture` scanned as `stack`; returns
/// the stack's report (`None` when its tool is missing).
fn check_fixture(fixture: &str, stack: &str) -> Option<Value> {
    check_fixture_doc(fixture, stack).map(|(report, _)| report)
}

/// `check_fixture` that also returns the whole JSON document.
fn check_fixture_doc(fixture: &str, stack: &str) -> Option<(Value, Value)> {
    let r = repo(fixture);
    let (code, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", stack]);
    let report = v["stacks"].as_array().unwrap().iter().find(|s| s["stack"] == stack).cloned().expect("stack report");
    if report["status"] == "skipped" {
        eprintln!("{stack}: skipped ({})", report["message"]);
        return None;
    }
    assert_eq!(report["status"], "ok", "{stack}: {}", report["message"]);
    let findings = v["findings"].as_array().unwrap();
    let mut problems = Vec::new();
    for (file, line, marker) in markers(fixture) {
        let here: Vec<&Value> = findings.iter().filter(|f| f["file"] == file.as_str() && f["line"] == line).collect();
        if marker.is_empty() {
            if !here.is_empty() {
                problems.push(format!("{file}:{line} should be clean, got {:?}", here.iter().map(|f| f["rule"].clone()).collect::<Vec<_>>()));
            }
            continue;
        }
        let (rule, kind) = marker.rsplit_once(':').unwrap();
        if !here.iter().any(|f| f["rule"].as_str().unwrap().contains(rule) && f["kind"] == kind) {
            problems.push(format!(
                "{file}:{line} wants {rule} ({kind}), got {:?}",
                here.iter().map(|f| format!("{} {}", f["rule"], f["kind"])).collect::<Vec<_>>()
            ));
        }
        if kind == "needs-newer" && !here.iter().any(|f| f["upgrade"].is_object()) {
            problems.push(format!("{file}:{line} needs-newer without upgrade"));
        }
    }
    assert!(problems.is_empty(), "{stack}:\n{}\n{}", problems.join("\n"), serde_json::to_string_pretty(&v).unwrap());
    assert_eq!(code, 1, "findings mean exit 1");
    // Nothing written into the scanned repository (no obj/, caches, reports).
    let status = Command::new("git").args(["status", "--porcelain", "--ignored", "--untracked-files=all"]).current_dir(&r.0).output().unwrap();
    assert!(status.stdout.is_empty(), "{stack} wrote into the repo: {}", String::from_utf8_lossy(&status.stdout));
    Some((report, v))
}

#[test]
fn python_fixture() {
    check("python");
}

#[test]
fn rust_fixture() {
    check("rust");
}

/// Edition 2021 + rust-version 1.70: let chains (`collapsible_if`), `LazyLock`
/// and `Duration::from_mins` only show up as needs-newer.
#[test]
fn rust_raised_pass_fixture() {
    let Some(report) = check_fixture("rust-raised", "rust") else { return };
    let message = report["message"].as_str().unwrap();
    assert!(message.contains("raised copy (edition 2024, rust-version 1.100) compiled"), "{message}");
}

/// The csharp fixture targets net8.0; with SDK 11 installed the stack must
/// not claim the .NET 11 SDK is missing. A8: the report lists the IDE/CA ids.
#[test]
fn csharp_fixture() {
    let Some(report) = check_fixture("csharp", "csharp") else { return };
    let sdks = Command::new(installed("dotnet").unwrap()).arg("--list-sdks").output().unwrap();
    let has_11 = String::from_utf8_lossy(&sdks.stdout).lines().any(|l| l.split('.').next().and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| m >= 11));
    let message = report["message"].as_str().unwrap();
    if has_11 {
        assert!(!message.contains("need the .NET 11 SDK"), "SDK 11 is installed: {message}");
    }
    let rules: Vec<&str> = report["rules"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    for id in ["IDE0090", "IDE0300", "CA1510", "SYSLIB1045", "csharp-new-random"] {
        assert!(rules.contains(&id), "{id} in {rules:?}");
    }
}

#[test]
fn js_and_ts_fixture() {
    check("js");
}

/// css/prefer-logical-properties: high only for horizontal physical
/// properties and `text-align: left|right`; `left`/`right` low; `width`, `top`, `vh` dropped;
/// other values (`float: left`) low.
#[test]
fn css_fixture() {
    let Some((report, v)) = check_fixture_doc("css", "css") else { return };
    let line = |n: u64| -> Vec<(String, String)> {
        v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["line"] == n)
            .map(|f| (f["rule"].as_str().unwrap().to_string(), f["confidence"].as_str().unwrap().to_string()))
            .collect()
    };
    let logical = "css/prefer-logical-properties".to_string();
    assert_eq!(line(12), vec![(logical.clone(), "high".to_string())], "{v}");
    // `right: 0` (a positioned box's inset) only warns.
    let (high, low) = ((logical.clone(), "high".to_string()), (logical.clone(), "low".to_string()));
    assert_eq!(line(13), vec![high.clone(), high, low], "{v}");
    assert_eq!(line(14), vec![(logical, "low".to_string())], "{v}");
    let rules: Vec<&str> = report["rules"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    for id in ["css/prefer-logical-properties", "color-function-notation", "plugin/use-baseline"] {
        assert!(rules.contains(&id), "{id} in {rules:?}");
    }
}

#[test]
fn powershell_fixture() {
    check("powershell");
}

/// go 1.21: `interface{}` is modern, `strings.Split` ranges need 1.24, the
/// three-clause loop 1.22 and the ioutil inline the toolchain's own Go; the
/// Linux-only file is found by the GOOS=linux pass on other hosts. Skipped
/// (the stack says why) when Go 1.26 or newer is not installed.
#[test]
fn go_fixture() {
    let Some((report, v)) = check_fixture_doc("go", "go") else { return };
    let tool = report["tool"].as_str().unwrap();
    assert!(tool.ends_with(" cmd/fix"), "{tool}");
    let message = report["message"].as_str().unwrap();
    if !cfg!(target_os = "linux") {
        assert!(message.contains("GOOS=linux pass analysed 1 file(s)"), "{message}");
    }
    assert!(message.contains("never builds tools/gen.go"), "{message}");
    assert_eq!(report["pins"]["go"]["text.go"], "1.21 (go.mod)");
    let findings = v["findings"].as_array().unwrap();
    let find = |rule: &str| findings.iter().find(|f| f["rule"] == rule).unwrap_or_else(|| panic!("no {rule}: {v}"));
    let to = |rule: &str| -> String {
        let f = find(rule);
        assert_eq!(f["upgrade"]["pin"], "go.mod go", "{f}");
        f["upgrade"]["to"].as_str().unwrap().to_string()
    };
    assert_eq!(to("go/rangeint"), "1.22");
    assert_eq!(to("go/stringsseq"), "1.24");
    assert_eq!(to("go/omitzero"), "1.24");
    assert_eq!(find("go/omitzero")["confidence"], "low", "omitzero changes encoding/json output");
    assert_eq!(to("go-math-rand-v2"), "1.22");
    // std calls the inliner migrates: one catalogue finding each. The only
    // go/inline findings are the non-std callees declared with go1.24, one of
    // them on a line a low catalogue rule (ioutil.ReadDir) also matched.
    let inline: Vec<&Value> = findings.iter().filter(|f| f["rule"] == "go/inline").collect();
    assert_eq!(inline.len(), 2, "{inline:?}");
    for f in &inline {
        assert_eq!((f["kind"].as_str(), f["upgrade"]["to"].as_str()), (Some("needs-newer"), Some("1.24")), "{f}");
    }
    let readdir = find("go-ioutil-readdir");
    assert!(inline.iter().any(|f| f["line"] == readdir["line"]), "the low rule's line keeps its go/inline: {v}");
    let read_line = findings.iter().filter(|f| f["file"] == "text.go" && f["rule"] == "go-ioutil-io-os").collect::<Vec<_>>();
    assert_eq!(read_line.len(), 1, "{v}");
    let line = read_line[0]["line"].clone();
    let here: Vec<&Value> = findings.iter().filter(|f| f["file"] == "text.go" && f["line"] == line).collect();
    assert_eq!(here.len(), 1, "one finding on the ioutil.ReadAll line: {here:?}");
    assert_eq!((here[0]["kind"].as_str(), here[0]["confidence"].as_str()), (Some("modern"), Some("high")));
}

/// A go.work with two modules (`app` go 1.22, `lib` go 1.16): `app` imports
/// `lib` through the workspace, and an untouched package of `app` embeds a
/// folder git does not track. Both changes are found and nothing fails.
#[test]
fn go_workspace_fixture() {
    let Some((report, v)) = check_fixture_doc("go-work", "go") else { return };
    let message = report["message"].as_str().unwrap();
    assert!(!message.contains("failed"), "{message}");
    let lib: Vec<&Value> = v["findings"].as_array().unwrap().iter().filter(|f| f["file"] == "lib/lib.go").collect();
    assert_eq!(lib.len(), 1, "go 1.16: ioutil.ReadAll is one modern high finding, no needs-newer: {lib:?}");
    assert_eq!((lib[0]["rule"].as_str(), lib[0]["kind"].as_str(), lib[0]["confidence"].as_str()), (Some("go-ioutil-io-os"), Some("modern"), Some("high")));
    assert_eq!(report["pins"]["go"]["lib/lib.go"], "1.16 (lib/go.mod)");
    // The raised copy raises go.work too and runs without -mod=mod in a workspace.
    assert!(message.contains("app: raised copy (go "), "{message}");
    let seq = v["findings"].as_array().unwrap().iter().find(|f| f["file"] == "app/main.go" && f["rule"] == "go/stringsseq").unwrap_or_else(|| panic!("{v}"));
    assert_eq!((seq["kind"].as_str(), seq["upgrade"]["to"].as_str()), (Some("needs-newer"), Some("1.24")), "{seq}");
}

/// A change to a package whose `//go:embed` target git does not track leaves
/// that package out (named in `message`); the other changed package is analysed.
#[test]
fn go_package_go_cannot_load_is_left_out_not_fatal() {
    let r = files_repo(
        "go-embed",
        &[("go.mod", "module example.com/e\n\ngo 1.21\n"), (".gitignore", "web/dist/\n")],
        &[("web/web.go", "package web\n\nimport \"embed\"\n\n//go:embed dist\nvar FS embed.FS\n"), ("api/api.go", "package api\n\nfunc F(v interface{}) {}\n")],
    );
    let (_, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go"]);
    let Some(report) = go_report(&v) else { return };
    let message = report["message"].as_str().unwrap();
    assert_eq!(report["status"], "ok", "{message}");
    assert!(message.contains("example.com/e/web not analysed") && message.contains("pattern dist: no matching files found"), "{message}");
    assert!(message.contains("build them before the scan"), "{message}");
    assert!(v["findings"].as_array().unwrap().iter().any(|f| f["file"] == "api/api.go" && f["rule"] == "go/any"), "{v}");
}

/// The only changed package type-checks on Linux alone (`log/syslog` has no
/// constants on Windows): the host pass fails, the GOOS=linux pass finds it.
#[test]
fn go_package_that_type_checks_only_on_linux_is_retried_there() {
    let r = files_repo(
        "go-syslog",
        &[("go.mod", "module example.com/m\n\ngo 1.21\n")],
        &[("svc/svc.go", "package svc\n\nimport \"log/syslog\"\n\nvar Level = syslog.LOG_INFO\n\nfunc F(v interface{}) {}\n")],
    );
    let (_, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go"]);
    let Some(report) = go_report(&v) else { return };
    assert_eq!(report["status"], "ok", "{}", report["message"]);
    assert!(v["findings"].as_array().unwrap().iter().any(|f| f["file"] == "svc/svc.go" && f["line"] == 7 && f["rule"] == "go/any"), "{v}");
}

/// A changed unconstrained package imports a Linux-only internal package: go
/// cannot load it on other hosts, so the GOOS=linux pass analyses it, alone
/// and next to a portable change.
#[test]
fn go_package_importing_linux_only_code_is_retried_on_linux() {
    let base = [
        ("go.mod", "module example.com/m\n\ngo 1.21\n"),
        ("lnx/lnx.go", "//go:build linux\n\npackage lnx\n\n// Name is Linux-only.\nfunc Name() string { return \"lnx\" }\n"),
    ];
    let svc = ("svc/svc.go", "package svc\n\nimport \"example.com/m/lnx\"\n\nvar N = lnx.Name\n\nfunc F(v interface{}) {}\n");
    let api = ("api/api.go", "package api\n\nfunc G(v interface{}) {}\n");
    for (name, change) in [("go-lnx", vec![svc]), ("go-lnx-api", vec![svc, api])] {
        let r = files_repo(name, &base, &change);
        let (_, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go"]);
        let Some(report) = go_report(&v) else { return };
        assert_eq!(report["status"], "ok", "{name}: {}", report["message"]);
        for (file, _) in &change {
            assert!(v["findings"].as_array().unwrap().iter().any(|f| f["file"] == *file && f["rule"] == "go/any"), "{name}: {file}: {v}");
        }
    }
}

/// Without the catalogue (a home with no rules/go.yml), a std call the
/// inliner refuses below the toolchain's go line is one `modern` finding.
#[test]
fn go_std_inline_without_a_rule_is_modern_with_its_note() {
    let r = files_repo(
        "go-std-inline",
        &[("go.mod", "module example.com/s\n\ngo 1.21\n")],
        &[("a.go", "package s\n\nimport (\n\t\"io\"\n\t\"io/ioutil\"\n)\n\nfunc R(r io.Reader) []byte { b, _ := ioutil.ReadAll(r); return b }\n")],
    );
    let home = PathBuf::from(format!("{}-home", r.0.display()));
    fs::create_dir_all(&home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_modern-tara"))
        .args(["scan", "--range", "HEAD~1..HEAD", "--stacks", "go", "--quiet", "--repo"])
        .arg(&r.0)
        .env("MODERN_TARA_HOME", &home)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&home);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("bad JSON ({e}): {}", String::from_utf8_lossy(&out.stderr)));
    if go_report(&v).is_none() {
        return;
    }
    let findings = v["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{v}");
    let f = &findings[0];
    assert_eq!((f["rule"].as_str(), f["kind"].as_str(), f["confidence"].as_str()), (Some("go/inline"), Some("modern"), Some("high")), "{f}");
    assert!(f["message"].as_str().unwrap().contains("std replacement exists; go fix applies it once the go line reaches"), "{f}");
}

/// `--timeout` is one budget for all go calls: one second is not enough for
/// the fixture's passes, so the stack fails and says it timed out.
#[test]
fn go_timeout_is_one_budget_and_fails_the_stack() {
    let r = repo("go");
    let (code, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go", "--timeout", "1"]);
    let Some(report) = go_report(&v) else { return };
    assert_eq!(report["status"], "error", "{v}");
    assert!(report["message"].as_str().unwrap().contains("timed out after 1s"), "{v}");
    assert_ne!(code, 0);
}

/// A repository from `(path, contents)` pairs: `base` as the first commit and,
/// when `change` is not empty, `change` as the second.
fn files_repo(name: &str, base: &[(&str, &str)], change: &[(&str, &str)]) -> TempRepo {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mt-test-{name}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let guard = TempRepo(dir.clone());
    let write = |files: &[(&str, &str)]| {
        for (p, body) in files {
            let f = dir.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, body).unwrap();
        }
    };
    write(base);
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "--no-verify", "-m", "base"]);
    if !change.is_empty() {
        write(change);
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "--no-verify", "-m", "change"]);
    }
    guard
}

fn go_report(v: &Value) -> Option<Value> {
    let report = v["stacks"].as_array().unwrap().iter().find(|s| s["stack"] == "go").cloned().expect("go report");
    if report["status"] == "skipped" {
        eprintln!("go: skipped ({})", report["message"]);
        return None;
    }
    Some(report)
}

/// The user's environment cannot change the run: GOOS/GOARCH (a linux/386
/// GOEXE has no `.exe`, darwin/386 does not exist), GOFLAGS, GO111MODULE,
/// GOWORK and GOTOOLCHAIN are all replaced for every go call.
#[test]
fn go_ignores_the_users_go_environment() {
    let r = files_repo(
        "go-env",
        &[
            ("go.mod", "module example.com/env\n\ngo 1.21\n"),
            ("a.go", "package env\n\nfunc F(v interface{}) {}\n"),
            ("d.go", "//go:build darwin\n\npackage env\n\nfunc G(v interface{}) {}\n"),
        ],
        &[],
    );
    let bogus_work = r.0.join("missing").join("go.work");
    let out = Command::new(env!("CARGO_BIN_EXE_modern-tara"))
        .args(["scan", "--all-lines", "--stacks", "go", "--quiet", "--repo"])
        .arg(&r.0)
        .env("MODERN_TARA_HOME", env!("CARGO_MANIFEST_DIR"))
        .env("GOOS", "linux")
        .env("GOARCH", "386")
        .env("GOFLAGS", "-mod=vendor")
        .env("GO111MODULE", "off")
        .env("GOWORK", &bogus_work)
        .env("GOTOOLCHAIN", "go1.99.0")
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("bad JSON ({e}): {}", String::from_utf8_lossy(&out.stderr)));
    let report = v["stacks"].as_array().unwrap().iter().find(|s| s["stack"] == "go").cloned().expect("go report");
    let message = report["message"].as_str().unwrap_or_default();
    if report["status"] == "skipped" && (message.starts_with("go not found") || message.contains("has no `go fix` modernizers")) {
        eprintln!("go: skipped ({message})");
        return;
    }
    assert_eq!(report["status"], "ok", "{message}");
    for (file, line) in [("a.go", 3), ("d.go", 5)] {
        assert!(v["findings"].as_array().unwrap().iter().any(|f| f["file"] == file && f["line"] == line && f["rule"] == "go/any"), "{file}:{line}: {v}");
    }
}

/// A change only under `vendor/` is never analysed: ast-grep included.
#[test]
fn go_vendored_change_does_not_stop_the_gate() {
    let dep = |body: &str| {
        format!("package dep\n\nimport (\n\t\"io\"\n\t\"io/ioutil\"\n\t\"sort\"\n)\n\nvar W io.Writer = ioutil.Discard\n\nfunc S(x []string) {{\n{body}}}\n")
    };
    let r = files_repo(
        "go-vendor",
        &[
            ("go.mod", "module example.com/v\n\ngo 1.21\n\nrequire example.com/dep v1.0.0\n"),
            ("main.go", "package main\n\nimport _ \"example.com/dep\"\n\nfunc main() {}\n"),
            ("vendor/modules.txt", "# example.com/dep v1.0.0\n## explicit\nexample.com/dep\n"),
            ("vendor/example.com/dep/dep.go", &dep("")),
        ],
        &[("vendor/example.com/dep/dep.go", &dep("\tsort.Strings(x)\n"))],
    );
    let (code, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go", "--gate"]);
    if go_report(&v).is_none() {
        return;
    }
    assert!(v["findings"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(code, 0, "{v}");
}

/// A changed file only a build tag or another GOOS includes is analysed by an
/// extra pass (`-tags`, `GOOS=freebsd`), never left unscanned; `ignore` stays out.
#[test]
fn go_files_behind_tags_or_other_goos_are_analysed() {
    let any = |pkg: &str| format!("package {pkg}\n\nfunc F(v interface{{}}) {{}}\n");
    let r = files_repo(
        "go-tags",
        &[("go.mod", "module example.com/t\n\ngo 1.21\n"), ("a.go", "package t\n")],
        &[
            ("it/it.go", &format!("//go:build integration\n\n{}", any("it"))),
            ("tools/tools.go", &format!("//go:build tools\n\n{}", any("tools"))),
            ("bsd/x_freebsd.go", &any("bsd")),
            ("gen/gen.go", &format!("//go:build ignore\n\n{}", any("main"))),
        ],
    );
    let (code, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go", "--gate"]);
    let Some(report) = go_report(&v) else { return };
    assert_eq!(report["status"], "ok", "{}", report["message"]);
    for file in ["it/it.go", "tools/tools.go", "bsd/x_freebsd.go"] {
        assert!(v["findings"].as_array().unwrap().iter().any(|f| f["file"] == file && f["rule"] == "go/any"), "{file}: {v}");
    }
    assert_eq!(unscanned(&v, "go"), vec![], "{v}");
    assert_eq!(code, 1, "findings, not a false stop: {v}");
}

/// More than three extra (GOOS, tags) passes per module: the files beyond the
/// cap are unscanned and the reason says so.
#[test]
fn go_extra_passes_are_capped_at_three() {
    let any = |tag: &str| format!("//go:build {tag}\n\npackage t\n\nfunc F{tag}(v interface{{}}) {{}}\n");
    let files: Vec<(String, String)> = (1..=4).map(|i| (format!("t{i}.go"), any(&format!("t{i}")))).collect();
    let change: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
    let r = files_repo("go-cap", &[("go.mod", "module example.com/c\n\ngo 1.21\n"), ("a.go", "package t\n")], &change);
    let (_, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go", "--gate"]);
    let Some(report) = go_report(&v) else { return };
    assert_eq!(report["status"], "ok", "{}", report["message"]);
    let list = unscanned(&v, "go");
    assert_eq!(list.len(), 1, "{v}");
    assert_eq!(list[0].0, "t4.go");
    assert!(list[0].1.contains("more than 3 extra"), "{list:?}");
    let analysed = v["findings"].as_array().unwrap().iter().filter(|f| f["rule"] == "go/any").count();
    assert_eq!(analysed, 3, "{v}");
}

/// A go line newer than the toolchain is an error with the reason, never a download.
#[test]
fn go_module_newer_than_the_toolchain_is_an_error() {
    let r = files_repo("go-new", &[("go.mod", "module example.com/n\n\ngo 1.99\n"), ("a.go", "package n\n\nfunc F(v interface{}) {}\n")], &[]);
    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "go"]);
    let Some(report) = go_report(&v) else { return };
    assert_eq!(report["status"], "error", "{v}");
    assert!(report["message"].as_str().unwrap().contains("never downloads a toolchain"), "{v}");
    assert_eq!(code, 2);
}

#[test]
fn plain_folder_with_files_and_all_lines() {
    let dir = std::env::temp_dir().join(format!("mt-test-plain-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let guard = TempRepo(dir.clone());
    fs::write(dir.join("bench.py"), "import os\nx = os.path.join('a', 'b')\n").unwrap();
    let (code, v) = scan(&guard.0, &["--files", "bench.py", "--all-lines"]);
    assert_eq!(v["change"]["mode"], "files");
    let report = &v["stacks"][0];
    if report["status"] == "skipped" {
        eprintln!("js: skipped ({})", report["message"]);
        return;
    }
    assert!(v["findings"].as_array().unwrap().iter().any(|f| f["rule"] == "PTH118" && f["line"] == 2), "{v}");
    assert_eq!(code, 1);
    let entries: Vec<_> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(entries.len(), 1, "plain folder gained files: {entries:?}");
}

/// Turkish names: a folder with `ş`, a file with `İ` (lowercase is longer
/// in bytes) given as an absolute path. Used to panic on a byte cut.
#[test]
fn turkish_absolute_files_path_in_plain_folder() {
    let dir = std::env::temp_dir().join(format!("mt-rev-ş-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let guard = TempRepo(dir.clone());
    let file = dir.join("İİ.py");
    fs::write(&file, "import os\nx = os.path.join('a', 'b')\n").unwrap();
    let file_arg = file.to_string_lossy().into_owned();
    let (code, v) = scan(&guard.0, &["--files", &file_arg, "--all-lines"]);
    assert_ne!(code, 101);
    assert_eq!(v["stacks"][0]["files"][0], "İİ.py", "{v}");
}

/// Probes run from a neutral folder: a bogus rust-toolchain.toml in the
/// caller's folder must not make rustup install or fail.
#[test]
fn version_ignores_callers_toolchain_file() {
    let dir = std::env::temp_dir().join(format!("mt-toolchain-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let guard = TempRepo(dir);
    fs::write(guard.0.join("rust-toolchain.toml"), "[toolchain]\nchannel = \"1.0.0-modern-tara-bogus\"\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_modern-tara")).arg("--version").current_dir(&guard.0).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let rustc = text.lines().find(|l| l.trim_start().starts_with("rustc ")).unwrap_or_default();
    if rustc.contains("not found") {
        return;
    }
    assert!(rustc.contains("rustc 1.") && !rustc.contains("bogus") && !rustc.contains("failed"), "{text}");
}

/// The tool's own sources use the modern forms it reports.
#[test]
fn own_sources_have_no_strip_unwrap_or_and_no_regex_replace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(exe) = installed("ast-grep") {
        let out = Command::new(exe)
            .args(["scan", "--config", "sgconfig.yml", "--filter", "rust-strip-unwrap-or-trim", "--json=stream", "src"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    }
    for f in ["js/modern-tara.mjs", "js/modern-tara.eslint.mjs"] {
        let text = fs::read_to_string(root.join(f)).unwrap();
        // `p.replace(/\\/g, '/')` is unicorn prefer-string-replace-all.
        assert!(!text.contains(r"replace(/\\/g"), "{f} still uses a regex replace instead of replaceAll");
    }
}

/// `--repo <subdir>` spelled in another case still limits to that subdir
/// (Windows: paths ignore case there, so `SUB` is the folder `sub`).
#[cfg(windows)]
#[test]
fn repo_subdir_in_other_case_limits_files() {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mt-test-subdir-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("sub")).unwrap();
    let guard = TempRepo(dir.clone());
    for f in ["top.py", "sub/a.py"] {
        fs::write(dir.join(f), "import os\nx = os.path.join('a', 'b')\n").unwrap();
    }
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "--no-verify", "-m", "one"]);
    let (code, v) = scan(&guard.0.join("SUB"), &["--all-lines", "--stacks", "python"]);
    assert_eq!(v["stacks"][0]["files"], serde_json::json!(["sub/a.py"]), "{v}");
    if v["stacks"][0]["status"] == "ok" {
        assert_eq!(code, 1);
    }
}

#[test]
fn not_a_repo_without_all_lines_is_a_tool_error() {
    let dir = std::env::temp_dir().join(format!("mt-test-norepo-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let guard = TempRepo(dir);
    let out = Command::new(env!("CARGO_BIN_EXE_modern-tara")).arg("scan").arg("--repo").arg(&guard.0).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn clean_change_exits_zero_with_shape() {
    let r = repo("python");
    // Only the base commit's untouched line differs from nothing: scan the
    // index against HEAD, which is empty.
    let (code, v) = scan(&r.0, &["--staged"]);
    assert_eq!(code, 0, "{v}");
    for key in ["tool", "version", "repo", "change", "stacks", "findings"] {
        assert!(v.get(key).is_some(), "missing {key}");
    }
    assert_eq!(v["tool"], "modern-tara");
    assert_eq!(v["findings"].as_array().unwrap().len(), 0);
}

/// With `engines.node >=22` the `reduce` grouping is a plain `modern` finding.
#[test]
fn js_group_by_is_modern_on_node_22() {
    let r = repo("js");
    fs::write(
        r.0.join("package.json"),
        "{\n  \"name\": \"fixture-js\",\n  \"private\": true,\n  \"type\": \"commonjs\",\n  \"engines\": { \"node\": \">=22\" }\n}\n",
    )
    .unwrap();
    git(&r.0, &["add", "-A"]);
    git(&r.0, &["commit", "-q", "--no-verify", "-m", "raise node"]);
    let (_, v) = scan(&r.0, &["--range", "HEAD~2..HEAD", "--stacks", "js"]);
    let report = v["stacks"].as_array().unwrap().iter().find(|s| s["stack"] == "js").cloned().expect("stack report");
    if report["status"] == "skipped" {
        eprintln!("js: skipped ({})", report["message"]);
        return;
    }
    let found = v["findings"].as_array().unwrap().iter().any(|f| f["rule"] == "unicorn/prefer-group-by" && f["kind"] == "modern");
    assert!(found, "{v}");
}

/// `--gate`: a high-confidence `modern` finding stops the gate (exit 1).
#[test]
fn gate_stops_on_high_confidence_modern() {
    let r = python_repo("gate-high", ">=3.12", "import os\n\nx = os.path.join('a', 'b')\n");
    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "python", "--gate"]);
    if v["stacks"][0]["status"] != "ok" {
        eprintln!("python: {}", v["stacks"][0]["message"]);
        return;
    }
    let findings = v["findings"].as_array().unwrap();
    assert!(findings.iter().any(|f| f["kind"] == "modern" && f["confidence"] == "high"), "{v}");
    assert_eq!(code, 1, "{v}");
}

/// `--gate`: low-confidence and needs-newer findings are warnings (exit 0);
/// without `--gate` the same findings give exit 1.
#[test]
fn gate_passes_on_low_confidence_and_needs_newer() {
    let cases = [
        // `asyncio.gather` is a low-confidence catalogue heuristic (modern on 3.12).
        ("gate-low", ">=3.12", "import asyncio\n\n\nasync def both(a, b):\n    return await asyncio.gather(a, b)\n"),
        // `List[int]` needs Python 3.9 (UP006, UP035): needs-newer under >=3.8.
        ("gate-newer", ">=3.8", "from typing import List\n\n\ndef ints() -> List[int]:\n    return [1]\n"),
    ];
    for (name, requires, code_text) in cases {
        let r = python_repo(name, requires, code_text);
        let (gated, v) = scan(&r.0, &["--all-lines", "--stacks", "python", "--gate"]);
        if v["stacks"][0]["status"] != "ok" {
            eprintln!("python: {}", v["stacks"][0]["message"]);
            return;
        }
        let findings = v["findings"].as_array().unwrap();
        for f in findings {
            assert!(f["confidence"] == "high" || f["confidence"] == "low", "{f}");
            assert!(!(f["kind"] == "modern" && f["confidence"] == "high"), "{name}: unexpected gate finding {f}");
        }
        if name == "gate-newer" {
            assert!(findings.iter().any(|f| f["kind"] == "needs-newer"), "{v}");
        }
        if let Some(f) = findings.iter().find(|f| f["rule"] == "python-asyncio-gather") {
            assert_eq!((f["kind"].as_str(), f["confidence"].as_str()), (Some("modern"), Some("low")), "{f}");
        } else if name == "gate-low" && installed("ast-grep").is_some() {
            panic!("ast-grep is installed but python-asyncio-gather is missing: {v}");
        }
        assert_eq!(gated, 0, "{name}: {v}");
        let (plain, _) = scan(&r.0, &["--all-lines", "--stacks", "python"]);
        assert_eq!(plain, i32::from(!findings.is_empty()), "{name}: without --gate every finding counts");
    }
}

const OLD_JOIN: &str = "import os\n\nx = os.path.join('a', 'b')\n";
const JOIN_RULE: &str = "PTH118";

fn waiver_file(extra: &str) -> String {
    format!("[[waiver]]\nrule = \"{JOIN_RULE}\"\npaths = [\"*.py\"]\nreason = \"legacy paths\"\ncard = \"MT7\"\n{extra}")
}

/// Whether the python stack of a `--stacks python` run did not run (tool missing).
fn python_unusable(v: &Value) -> bool {
    let stack = &v["stacks"][0];
    let skipped = stack["status"] != "ok";
    if skipped {
        eprintln!("python: {}", stack["message"]);
    }
    skipped
}

fn rules_of(list: &Value) -> Vec<String> {
    list.as_array().unwrap().iter().map(|f| f["rule"].as_str().unwrap().to_string()).collect()
}

fn warnings_of(v: &Value) -> Vec<String> {
    v["warnings"].as_array().unwrap().iter().map(|w| w.as_str().unwrap().to_string()).collect()
}

#[test]
fn waiver_takes_a_finding_out_of_the_count() {
    let r = python_repo_with("waive", ">=3.12", OLD_JOIN, &[("modern-tara.toml", &waiver_file(""))]);
    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "python"]);
    if python_unusable(&v) {
        return;
    }
    assert_eq!(v["findings"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(rules_of(&v["waived"]), vec![JOIN_RULE], "{v}");
    assert_eq!(v["waived"][0]["waiver"], serde_json::json!({"index": 1, "rule": JOIN_RULE, "reason": "legacy paths", "card": "MT7"}));
    assert_eq!(v["waived"][0]["file"], "app.py");
    assert_eq!(code, 0, "{v}");
    let (gated, _) = scan(&r.0, &["--all-lines", "--stacks", "python", "--gate"]);
    assert_eq!(gated, 0);
    assert_eq!(v["waivers"]["file"], "modern-tara.toml");
    let entry = &v["waivers"]["entries"][0];
    assert_eq!((entry["matched"].as_u64(), entry["status"].as_str()), (Some(1), Some("active")), "{v}");
    assert!(warnings_of(&v).is_empty(), "{v}");
    let (_, _, stderr) = scan_raw(&r.0, &["--all-lines", "--stacks", "python"]);
    assert!(stderr.contains("modern-tara: 0 findings, 1 waived in "), "{stderr}");
}

#[test]
fn expired_waiver_counts_again_with_a_warning() {
    let r = python_repo_with("expired", ">=3.12", OLD_JOIN, &[("modern-tara.toml", &waiver_file("expires = \"2020-01-01\"\n"))]);
    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "python", "--gate"]);
    if python_unusable(&v) {
        return;
    }
    assert_eq!(rules_of(&v["findings"]), vec![JOIN_RULE], "{v}");
    assert_eq!(v["waived"].as_array().unwrap().len(), 0);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["waivers"]["entries"][0]["status"], "expired");
    assert_eq!(v["waivers"]["entries"][0]["expires"], "2020-01-01");
    assert_eq!(warnings_of(&v), vec![format!("waiver 1 ({JOIN_RULE}) expired on 2020-01-01 (card MT7); its findings count again")]);
    let (_, _, stderr) = scan_raw(&r.0, &["--all-lines", "--stacks", "python"]);
    assert!(stderr.contains("modern-tara: warning: waiver 1"), "{stderr}");
}

#[test]
fn no_waivers_ignores_the_file_and_waivers_reads_another() {
    let r = python_repo_with("nowaive", ">=3.12", OLD_JOIN, &[("modern-tara.toml", &waiver_file(""))]);
    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "python", "--no-waivers"]);
    if python_unusable(&v) {
        return;
    }
    assert_eq!(code, 1, "{v}");
    assert_eq!(rules_of(&v["findings"]), vec![JOIN_RULE]);
    assert_eq!(v["waived"], serde_json::json!([]));
    assert!(v["waivers"]["file"].is_null() && v["waivers"]["entries"].as_array().unwrap().is_empty(), "{v}");

    // Another file, outside the repository; the repository file has its own waiver, which is ignored.
    let bare = python_repo_with("otherfile", ">=3.12", OLD_JOIN, &[("modern-tara.toml", &waiver_file("").replace("legacy paths", "from the repository"))]);
    let outside = std::env::temp_dir().join(format!("mt-test-waivers-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&outside).unwrap();
    let _guard = TempRepo(outside.clone());
    let path = outside.join("elsewhere.toml");
    fs::write(&path, waiver_file("")).unwrap();
    let path_text = path.to_string_lossy().into_owned();
    let (code, v) = scan(&bare.0, &["--all-lines", "--stacks", "python", "--waivers", &path_text]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["waivers"]["file"], path_text.as_str());
    assert_eq!(rules_of(&v["waived"]), vec![JOIN_RULE]);
    assert_eq!(v["waived"][0]["waiver"]["reason"], "legacy paths", "the --waivers file replaces the repository list");

    let missing = outside.join("missing.toml").to_string_lossy().into_owned();
    let (code, stdout, stderr) = scan_raw(&bare.0, &["--all-lines", "--stacks", "python", "--waivers", &missing]);
    assert_eq!(code, 2, "{stdout}{stderr}");
    assert!(stderr.contains("--waivers") && stdout.is_empty(), "{stderr}");
    let (code, _, stderr) = scan_raw(&bare.0, &["--all-lines", "--waivers", &missing, "--no-waivers"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("exclude each other"), "{stderr}");
}

#[test]
fn waiver_whose_paths_do_not_match_waives_nothing() {
    let text = waiver_file("").replace("[\"*.py\"]", "[\"lib/*.py\"]");
    let r = python_repo_with("nomatch", ">=3.12", OLD_JOIN, &[("modern-tara.toml", &text)]);
    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "python"]);
    if python_unusable(&v) {
        return;
    }
    assert_eq!(code, 1, "{v}");
    assert_eq!(rules_of(&v["findings"]), vec![JOIN_RULE]);
    assert_eq!(v["waivers"]["entries"][0]["matched"], 0);
    assert_eq!(warnings_of(&v), vec![format!("waiver 1 ({JOIN_RULE}) matched nothing")], "{v}");

    // A diff scan: an unmatched waiver is normal and says nothing.
    fs::write(r.0.join("new.py"), OLD_JOIN).unwrap();
    let (code, v) = scan(&r.0, &["--stacks", "python"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["findings"][0]["file"], "new.py");
    assert!(warnings_of(&v).is_empty(), "{v}");
}

#[test]
fn malformed_waiver_file_is_a_tool_error_naming_the_entry() {
    let text = format!("{}\n[[waiver]]\nrule = \"X\"\npaths = [\"a\"]\nreason = \"r\"\ncolour = \"red\"\n", waiver_file(""));
    let r = python_repo_with("badwaiver", ">=3.12", OLD_JOIN, &[("modern-tara.toml", &text)]);
    let (code, stdout, stderr) = scan_raw(&r.0, &["--all-lines", "--stacks", "python"]);
    assert_eq!(code, 2, "{stdout}{stderr}");
    assert!(stdout.is_empty(), "no JSON on a tool error: {stdout}");
    assert!(stderr.contains("modern-tara.toml: waiver 2: unknown key `colour`"), "{stderr}");
    // The audit switch does not read the file at all.
    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "python", "--no-waivers"]);
    assert!(v["waivers"]["file"].is_null(), "{v}");
}

#[test]
fn waiver_is_read_from_the_scanned_revision() {
    let r = python_repo_with("revwaiver", ">=3.12", "x = 1\n", &[("modern-tara.toml", &waiver_file(""))]);
    fs::write(r.0.join("more.py"), OLD_JOIN).unwrap();
    git(&r.0, &["add", "-A"]);
    git(&r.0, &["commit", "-q", "--no-verify", "-m", "two"]);
    fs::remove_file(r.0.join("modern-tara.toml")).unwrap();

    let (code, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "python"]);
    if python_unusable(&v) {
        return;
    }
    assert_eq!(code, 0, "the committed waiver applies to the revision: {v}");
    assert_eq!(rules_of(&v["waived"]), vec![JOIN_RULE], "{v}");
    assert_eq!(v["waivers"]["file"], "modern-tara.toml");

    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "python"]);
    assert_eq!(code, 1, "the work tree has no waiver file: {v}");
    assert_eq!(rules_of(&v["findings"]), vec![JOIN_RULE]);
    assert!(v["waivers"]["file"].is_null());
}

#[test]
fn files_globs_with_classes_and_braces() {
    let extra = [("lib.py", OLD_JOIN), ("zeta.py", OLD_JOIN), ("notes.md", "x\n")];
    let r = python_repo_with("globs", ">=3.12", OLD_JOIN, &extra);
    let files_of = |v: &Value| v["stacks"][0]["files"].clone();

    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "python", "--files", "[a-l]*.py"]);
    assert_eq!(files_of(&v), serde_json::json!(["app.py", "lib.py"]), "{v}");
    assert!(v["change"].get("unmatched").is_none());
    assert_eq!(v["warnings"], serde_json::json!([]));

    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "python", "--files", "{app,lib}.py"]);
    assert_eq!(files_of(&v), serde_json::json!(["app.py", "lib.py"]), "{v}");

    // One glob matches, one does not: a warning, and the scan goes on.
    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "python", "--files", "a*.py", "*.rs"]);
    assert_eq!(files_of(&v), serde_json::json!(["app.py"]), "{v}");
    assert_eq!(v["change"]["unmatched"], serde_json::json!(["*.rs"]));
    assert_eq!(warnings_of(&v), vec!["--files *.rs matches nothing"]);
    if !python_unusable(&v) {
        assert_eq!(code, 1, "{v}");
    }
    let (_, _, stderr) = scan_raw(&r.0, &["--all-lines", "--stacks", "python", "--files", "a*.py", "*.rs"]);
    assert!(stderr.contains("modern-tara: warning: --files *.rs matches nothing"), "{stderr}");

    // Every glob unmatched: a tool error.
    let (code, stdout, stderr) = scan_raw(&r.0, &["--all-lines", "--stacks", "python", "--files", "*.rs", "[x-z].md"]);
    assert_eq!(code, 2, "{stdout}{stderr}");
    assert!(stderr.contains("--files matched nothing: *.rs, [x-z].md"), "{stderr}");
    // A plain path that does not exist is still its own error.
    let (code, _, stderr) = scan_raw(&r.0, &["--all-lines", "--files", "gone.py"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("--files gone.py: no such file"), "{stderr}");
}

/// A repository of plain files, one commit.
fn plain_repo(name: &str, files: &[(&str, &str)]) -> TempRepo {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mt-test-{name}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let guard = TempRepo(dir.clone());
    for (path, content) in files {
        let target = dir.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, content).unwrap();
    }
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "--no-verify", "-m", "one"]);
    guard
}

/// Browser code: a directive, a top-level promise chain, and two constructs
/// that have nothing to do with modules (`var`, `items[items.length - 1]`).
const BROWSER_SCRIPT: &str = "'use strict';\n\nfetch('/api/items')\n  .then((response) => response.json())\n  .catch((error) => console.error(error));\n\nvar items = [1, 2, 3];\nvar last = items[items.length - 1];\n";
const NODE_SCRIPT: &str = "const fs = require('node:fs');\n\nmodule.exports = fs.readFileSync('x', 'utf8');\n";

fn js_files(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut files = vec![
        ("package.json", "{\n  \"name\": \"site\",\n  \"private\": true,\n  \"engines\": { \"node\": \">=24\" }\n}\n"),
        ("public/app.js", BROWSER_SCRIPT),
        ("server.js", NODE_SCRIPT),
    ];
    files.extend_from_slice(extra);
    files
}

/// `(rule, line)` of the findings in `file`.
fn findings_in(v: &Value, file: &str) -> Vec<(String, u64)> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["file"] == file)
        .map(|f| (f["rule"].as_str().unwrap().to_string(), f["line"].as_u64().unwrap()))
        .collect()
}

#[test]
fn classic_scripts_are_not_asked_to_become_modules() {
    let toml = "[js]\nclassic-scripts = [\"public/**/*.js\"]\n";
    let rules = |found: &[(String, u64)]| found.iter().map(|(rule, _)| rule.clone()).collect::<Vec<_>>();
    // Without the table: the module rules do flag the browser script.
    let plain = plain_repo("classic-off", &js_files(&[]));
    let (_, v) = scan(&plain.0, &["--all-lines", "--stacks", "js"]);
    if v["stacks"][0]["status"] != "ok" {
        eprintln!("js: skipped ({})", v["stacks"][0]["message"]);
        return;
    }
    let before = findings_in(&v, "public/app.js");
    assert!(before.contains(&("unicorn/prefer-module".to_string(), 1)), "{before:?}");
    for rule in ["unicorn/prefer-top-level-await", "no-var", "unicorn/prefer-at"] {
        assert!(rules(&before).iter().any(|r| r == rule), "{rule} in {before:?}");
    }
    let files = &v["stacks"][0]["pins"]["files"];
    assert_eq!((files["public/app.js"]["type"].as_str(), files["public/app.js"]["sourceType"].as_str()), (Some("commonjs"), Some("commonjs")));

    // With it: neither module rule, every other rule still there, the CommonJS server still flagged.
    let classic = plain_repo("classic-on", &js_files(&[("modern-tara.toml", toml)]));
    let (_, v) = scan(&classic.0, &["--all-lines", "--stacks", "js"]);
    let after = findings_in(&v, "public/app.js");
    let after_rules = rules(&after);
    for gone in ["unicorn/prefer-module", "unicorn/prefer-top-level-await", "unicorn/prefer-import-meta-properties"] {
        assert!(!after_rules.iter().any(|r| r == gone), "{gone} in {after:?}");
    }
    for stays in ["no-var", "unicorn/prefer-at"] {
        assert!(after_rules.iter().any(|r| r == stays), "{stays} must stay for a classic script: {after:?}");
    }
    assert!(findings_in(&v, "server.js").iter().any(|(rule, _)| rule == "unicorn/prefer-module"), "{v}");
    let files = &v["stacks"][0]["pins"]["files"];
    assert_eq!(files["public/app.js"]["sourceType"], "script", "{v}");
    assert_eq!(files["public/app.js"]["type"], "commonjs", "`type` stays the package kind: {v}");
    assert_eq!((files["server.js"]["type"].as_str(), files["server.js"]["sourceType"].as_str()), (Some("commonjs"), Some("commonjs")));
    assert_eq!(v["stacks"][0]["message"], "", "no note without .mjs/.cjs/TS files: {v}");
    assert_eq!(v["warnings"], serde_json::json!([]), "{v}");

    // `--no-waivers` keeps `[js]`.
    let (_, v) = scan(&classic.0, &["--all-lines", "--stacks", "js", "--no-waivers"]);
    assert_eq!(v["stacks"][0]["pins"]["files"]["public/app.js"]["sourceType"], "script", "{v}");

    // An `.mjs` the glob matches is still parsed as a module, and says nothing about it. A `.js` that is
    // really an ES module is linted as one (all rules) and raises a warning.
    let es_module = "import x from './mod.mjs';\n\nvar y = x;\nconsole.log(y);\n";
    let mixed_files = js_files(&[
        ("modern-tara.toml", "[js]\nclassic-scripts = [\"public/**\"]\n"),
        ("public/mod.mjs", "export const x = 1;\n"),
        ("public/bad.js", es_module),
    ]);
    let mixed = plain_repo("classic-mixed", &mixed_files);
    let (_, v) = scan(&mixed.0, &["--all-lines", "--stacks", "js"]);
    let message = v["stacks"][0]["message"].as_str().unwrap();
    assert!(message.contains("1 file(s) in classic-scripts keep their own module kind (.mjs/.cjs/TS)"), "{message}");
    let warnings: Vec<&str> = v["warnings"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
    assert_eq!(warnings, vec!["public/bad.js is in classic-scripts but parses only as an ES module"], "the .mjs must not be parsed as a script: {v}");
    assert!(rules(&findings_in(&v, "public/bad.js")).iter().any(|r| r == "no-var"), "module findings for bad.js: {v}");
    let files = &v["stacks"][0]["pins"]["files"];
    assert_eq!(files["public/mod.mjs"]["sourceType"], "module", "{v}");
    assert_eq!(files["public/bad.js"]["sourceType"], "module", "the kind that finally parsed: {v}");
    assert_eq!(files["public/app.js"]["sourceType"], "script", "{v}");
    // The gate sees the findings, and stderr carries the warning.
    let (code, _, stderr) = scan_raw(&mixed.0, &["--all-lines", "--stacks", "js", "--gate"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("modern-tara: warning: public/bad.js is in classic-scripts but parses only as an ES module"), "{stderr}");

    // The same file without the table is just retried as a module: findings, no warning.
    let retried = plain_repo("classic-retry", &js_files(&[("public/bad.js", es_module), ("public/mod.mjs", "export const x = 1;\n")]));
    let (_, v) = scan(&retried.0, &["--all-lines", "--stacks", "js"]);
    assert!(!v["stacks"][0]["message"].as_str().unwrap().contains("not parsed"), "{v}");
    assert_eq!(v["warnings"], serde_json::json!([]), "{v}");
    assert!(rules(&findings_in(&v, "public/bad.js")).iter().any(|r| r == "no-var"), "{v}");

    // Neither a script nor a module: a note, no warning.
    let garbage = plain_repo("classic-garbage", &js_files(&[("modern-tara.toml", toml), ("public/broken.js", "const = ;\n")]));
    let (_, v) = scan(&garbage.0, &["--all-lines", "--stacks", "js"]);
    assert!(v["stacks"][0]["message"].as_str().unwrap().contains("public/broken.js: not parsed as a classic script or as an ES module: "), "{v}");
    assert_eq!(v["warnings"], serde_json::json!([]), "{v}");

    // A broken `[js]` table is a tool error naming it.
    let broken = plain_repo("classic-broken", &js_files(&[("modern-tara.toml", "[js]\nclassic-scripts = []\n")]));
    let (code, stdout, stderr) = scan_raw(&broken.0, &["--all-lines", "--stacks", "js"]);
    assert_eq!(code, 2, "{stdout}{stderr}");
    assert!(stderr.contains("modern-tara.toml: [js]: "), "{stderr}");
}

/// No personal paths or user names in the published files. The licence's
/// copyright line is the one place a name belongs.
#[test]
fn no_personal_paths_or_user_names() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let placeholders = ["x", "runner", "user", "me", "you", "name"];
    let me: Vec<String> = ["USERNAME", "USER"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|u| u.to_lowercase())
        .filter(|u| u.len() >= 3 && !["runner", "runneradmin", "root", "user", "admin"].contains(&u.as_str()))
        .collect();
    let mut bad = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().unwrap().is_dir() {
                let skip = ["node_modules", ".git"].contains(&name.as_str()) || (d == root && ["target", "bin"].contains(&name.as_str()));
                if !skip {
                    dirs.push(e.path());
                }
                continue;
            }
            // `.git` is a file in a linked worktree (`gitdir: <main clone>/.git/worktrees/...`).
            if ["LICENSE", "Cargo.lock", "package-lock.json", ".git"].contains(&name.as_str()) {
                continue;
            }
            let Ok(text) = fs::read_to_string(e.path()) else { continue };
            let lower = text.to_lowercase();
            let rel = e.path().strip_prefix(root).unwrap().display().to_string();
            bad.extend(me.iter().filter(|u| lower.contains(u.as_str())).map(|_| format!("{rel}: names the current user")));
            for marker in ["users\\", "users/", "/home/"] {
                let mut from = 0;
                while let Some(at) = lower[from..].find(marker) {
                    from += at + marker.len();
                    let seg: String =
                        lower[from..].trim_start_matches(['\\', '/']).chars().take_while(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
                    if !seg.is_empty() && !placeholders.contains(&seg.as_str()) {
                        bad.push(format!("{rel}: {marker}{seg}"));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// `scan` with another tool home (`MODERN_TARA_HOME`); also returns stderr.
fn scan_home(dir: &Path, home: &Path, extra: &[&str]) -> (i32, Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_modern-tara"))
        .arg("scan")
        .arg("--repo")
        .arg(dir)
        .args(extra)
        .env("MODERN_TARA_CACHE", cache_for(dir))
        .env("MODERN_TARA_HOME", home)
        .output()
        .expect("modern-tara");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("bad JSON ({e}): {stderr}"));
    (out.status.code().unwrap_or(-1), v, stderr)
}

fn own_home() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A tool home with `sgconfig.yml`, `rules/` (plus `extra_rule` as
/// `rules/extra.yml` when given) and the `js/*.mjs` drivers, but no
/// `js/node_modules`: the js and css stacks are `skipped` there.
fn bare_home(name: &str, extra_rule: Option<&str>) -> TempRepo {
    let home = std::env::temp_dir().join(format!("mt-test-home-{name}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("rules")).unwrap();
    fs::create_dir_all(home.join("js")).unwrap();
    fs::copy(own_home().join("sgconfig.yml"), home.join("sgconfig.yml")).unwrap();
    copy_tree(&own_home().join("rules"), &home.join("rules"));
    for f in ["modern-tara.mjs", "modern-tara.eslint.mjs", "modern-tara.stylelint.mjs"] {
        fs::copy(own_home().join("js").join(f), home.join("js").join(f)).unwrap();
    }
    if let Some(rule) = extra_rule {
        fs::write(home.join("rules").join("extra.yml"), rule).unwrap();
    }
    TempRepo(home)
}

fn stack_of<'a>(v: &'a Value, stack: &str) -> &'a Value {
    v["stacks"].as_array().unwrap().iter().find(|s| s["stack"] == stack).unwrap_or_else(|| panic!("no {stack} report: {v}"))
}

/// `(file, reason)` of a stack's unscanned files.
fn unscanned(v: &Value, stack: &str) -> Vec<(String, String)> {
    stack_of(v, stack)["unscanned"]
        .as_array()
        .unwrap_or_else(|| panic!("{stack}: no unscanned list: {v}"))
        .iter()
        .map(|u| (u["file"].as_str().unwrap().to_string(), u["reason"].as_str().unwrap().to_string()))
        .collect()
}

fn rule_list(v: &Value, stack: &str) -> Vec<String> {
    stack_of(v, stack)["rules"].as_array().unwrap_or_else(|| panic!("{stack}: no rules: {v}")).iter().map(|r| r.as_str().unwrap().to_string()).collect()
}

/// A3: a skipped stack with touched files fails the gate (exit 2) unless
/// `--allow-skipped` names it; without `--gate` the exit stays 0.
#[test]
fn gate_fails_on_a_skipped_stack_unless_allowed() {
    let r = plain_repo("gate-skip", &[("package.json", "{\"name\":\"s\",\"private\":true}\n"), ("app.js", "const a = 1;\nconsole.log(a);\n")]);
    let home = bare_home("skip", None);
    let (code, v, stderr) = scan_home(&r.0, &home.0, &["--all-lines", "--stacks", "js", "--gate"]);
    assert_eq!(stack_of(&v, "js")["status"], "skipped", "{v}");
    assert_eq!(code, 2, "a skipped stack with a touched file is no pass: {stderr}");
    let list = unscanned(&v, "js");
    assert_eq!(list.len(), 1, "{v}");
    assert_eq!(list[0].0, "app.js");
    assert!(list[0].1.contains("eslint is not installed"), "{list:?}");
    assert!(stderr.contains("1 unscanned"), "{stderr}");
    for allow in ["js", "all", "python,js"] {
        let (code, v, stderr) = scan_home(&r.0, &home.0, &["--all-lines", "--stacks", "js", "--gate", "--allow-skipped", allow]);
        assert_eq!(code, 0, "--allow-skipped {allow}: {stderr}");
        assert_eq!(stack_of(&v, "js")["allowed"], true, "{v}");
    }
    let (code, _, _) = scan_home(&r.0, &home.0, &["--all-lines", "--stacks", "js", "--gate", "--allow-skipped", "python"]);
    assert_eq!(code, 2, "another stack's allowance does not cover js");
    let (code, _, _) = scan_home(&r.0, &home.0, &["--all-lines", "--stacks", "js"]);
    assert_eq!(code, 0, "without --gate a skipped stack keeps exit 0");
    let out = Command::new(env!("CARGO_BIN_EXE_modern-tara")).args(["scan", "--all-lines", "--allow-skipped", "cobol"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown stack: cobol"));
}

/// A3: a `.rs` file with no Cargo.toml above it is unscanned by clippy:
/// the gate fails and the report says why. Files no stack scans are
/// counted, never a failure; a stack with no touched file never fails.
#[test]
fn gate_fails_on_an_unscanned_file_and_lists_it() {
    if installed("cargo").is_none() {
        return;
    }
    let r = plain_repo("gate-loose", &[("scripts/tool.rs", "fn main() {}\n"), ("notes.html", "<p>x</p>\n"), ("site.scss", "a { b: c; }\n"), ("README", "x\n")]);
    let (code, v, stderr) = scan_home(&r.0, own_home(), &["--all-lines", "--gate"]);
    let list = unscanned(&v, "rust");
    assert_eq!(list.len(), 1, "{v}");
    assert_eq!(list[0].0, "scripts/tool.rs");
    assert!(list[0].1.contains("Cargo.toml"), "{list:?}");
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("1 unscanned"), "{stderr}");
    assert_eq!(v["no_stack"]["files"], 3, "{v}");
    assert_eq!(v["no_stack"]["extensions"], serde_json::json!({"(none)": 1, "html": 1, "scss": 1}), "{v}");
    assert!(stderr.contains("3 changed file(s) match no stack"), "{stderr}");
    let (code, _, _) = scan_home(&r.0, own_home(), &["--all-lines", "--gate", "--allow-skipped", "rust"]);
    assert_eq!(code, 0);
    let (code, _, _) = scan_home(&r.0, own_home(), &["--all-lines"]);
    assert_eq!(code, 0, "without --gate unscanned files are reported only");
    let (code, v, _) = scan_home(&r.0, own_home(), &["--all-lines", "--gate", "--stacks", "python"]);
    assert_eq!(code, 0, "only no-stack files and an unselected stack: {v}");
}

/// A3: an `ESLint` fatal parse error and a ruff syntax error are unscanned
/// files, not clean ones (and never a finding).
#[test]
fn parse_errors_are_unscanned_files() {
    let r = plain_repo(
        "parse-err",
        &[
            ("package.json", "{\"name\":\"p\",\"private\":true,\"type\":\"module\"}\n"),
            ("broken.js", "const = ;\n"),
            ("fine.js", "var x = 1;\nconsole.log(x);\n"),
            ("bad.py", "import os\nx = os.path.join('a', 'b')\ndef (:\n"),
            ("broken.ps1", "function f {\n  Get-Date\n"),
        ],
    );
    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "js,python,powershell", "--gate"]);
    if stack_of(&v, "powershell")["status"] == "ok" {
        let list = unscanned(&v, "powershell");
        assert_eq!(list.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), vec!["broken.ps1"], "{v}");
        assert!(findings_in(&v, "broken.ps1").is_empty(), "a parse error is never a finding: {v}");
    }
    if stack_of(&v, "js")["status"] == "ok" {
        let list = unscanned(&v, "js");
        assert_eq!(list.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), vec!["broken.js"], "{v}");
        assert!(list[0].1.contains("not parsed"), "{list:?}");
    }
    if stack_of(&v, "python")["status"] == "ok" {
        let list = unscanned(&v, "python");
        assert_eq!(list.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), vec!["bad.py"], "{v}");
        assert!(v["findings"].as_array().unwrap().iter().all(|f| f["rule"] != "invalid-syntax"), "{v}");
    }
    // `var` in fine.js is a high modern finding: 1 comes before 2.
    assert_eq!(code, 1, "{v}");
}

/// A3: a broken ast-grep rule file makes the stack an error, never clean.
#[test]
fn broken_ast_grep_rule_is_a_stack_error() {
    if installed("ast-grep").is_none() || installed("ruff").is_none() {
        return;
    }
    let home = bare_home("badrule", Some("id: broken\nlanguage: python\nrule:\n  patern: x\n"));
    let r = python_repo("sg-broken", ">=3.12", "x = 1\n");
    let (code, v, stderr) = scan_home(&r.0, &home.0, &["--all-lines", "--stacks", "python"]);
    let report = stack_of(&v, "python");
    assert_eq!(report["status"], "error", "{v}");
    assert!(report["message"].as_str().unwrap().contains("ast-grep"), "{v}");
    assert_eq!(code, 2, "{stderr}");
}

/// A3: `--all-lines` alone in a plain folder scans every file, build and
/// dependency folders left out; in a git repository also untracked files
/// that are not ignored.
#[test]
fn all_lines_alone_scans_every_file() {
    let dir = std::env::temp_dir().join(format!("mt-test-alldir-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&dir);
    let guard = TempRepo(dir.clone());
    for f in [
        "a.py",
        "sub/b.py",
        "node_modules/m.py",
        "dist/d.py",
        "build/x.py",
        ".venv/v.py",
        "__pycache__/c.py",
        "target/t.py",
        "obj/o.py",
        "bin/k.py",
        "src/bin/s.py",
    ] {
        let p = dir.join(f);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, OLD_JOIN).unwrap();
    }
    let (code, v) = scan(&guard.0, &["--all-lines", "--stacks", "python"]);
    assert_eq!(stack_of(&v, "python")["files"], serde_json::json!(["a.py", "src/bin/s.py", "sub/b.py"]), "{v}");
    if stack_of(&v, "python")["status"] == "ok" {
        assert_eq!(code, 1, "{v}");
    }

    let r = python_repo_with("all-untracked", ">=3.12", "x = 1\n", &[(".gitignore", "ignored.py\n")]);
    fs::write(r.0.join("new.py"), OLD_JOIN).unwrap();
    fs::write(r.0.join("ignored.py"), OLD_JOIN).unwrap();
    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "python"]);
    assert_eq!(stack_of(&v, "python")["files"], serde_json::json!(["app.py", "new.py"]), "{v}");
}

/// A7: a member crate of a workspace whose root is also a package is
/// linted; A8: the report lists the lints and catalogue rules that ran.
#[test]
fn rust_lints_workspace_members() {
    if installed("cargo").is_none() {
        return;
    }
    let r = plain_repo(
        "rust-ws",
        &[
            ("Cargo.toml", "[package]\nname = \"wsroot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\nmembers = [\"member\"]\n"),
            ("src/main.rs", "fn main() {}\n"),
            ("member/Cargo.toml", "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
            ("member/src/lib.rs", "pub fn show(s: &str) {\n    println!(\"{}\", s);\n}\n"),
        ],
    );
    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "rust"]);
    let report = stack_of(&v, "rust");
    if report["status"] == "skipped" {
        return;
    }
    assert_eq!(report["status"], "ok", "{v}");
    assert!(findings_in(&v, "member/src/lib.rs").contains(&("clippy::uninlined_format_args".to_string(), 2)), "{v}");
    let rules = rule_list(&v, "rust");
    for r in ["clippy::uninlined_format_args", "clippy::manual_*", "clippy::map_unwrap_or", "rust-oncelock-static-lazylock"] {
        assert!(rules.iter().any(|x| x == r), "{r} in {rules:?}");
    }
}

/// A7: inline suppressions do not hide findings: `# noqa` (ruff) and
/// `eslint-disable` comments; only modern-tara.toml waives.
#[test]
fn inline_suppressions_do_not_hide_findings() {
    let r = plain_repo(
        "suppress",
        &[
            ("pyproject.toml", "[project]\nname = \"s\"\nversion = \"0\"\nrequires-python = \">=3.12\"\n"),
            ("app.py", "import os\n\nx = os.path.join('a', 'b')  # noqa: PTH118\ny = os.path.join('c', 'd')  # noqa\n"),
            ("package.json", "{\"name\":\"s\",\"private\":true,\"type\":\"module\"}\n"),
            ("a.js", "/* eslint-disable */\nvar a = 1;\n// eslint-disable-next-line no-var\nvar b = 2;\nconsole.log(a, b);\n"),
        ],
    );
    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "python,js"]);
    if stack_of(&v, "python")["status"] == "ok" {
        let lines: Vec<u64> = findings_in(&v, "app.py").into_iter().filter(|(rule, _)| rule == JOIN_RULE).map(|(_, l)| l).collect();
        assert_eq!(lines, vec![3, 4], "{v}");
        let rules = rule_list(&v, "python");
        for r in ["UP", "FURB", "PTH", "SIM", "python-asyncio-gather"] {
            assert!(rules.iter().any(|x| x == r), "{r} in {rules:?}");
        }
    }
    if stack_of(&v, "js")["status"] == "ok" {
        let lines: Vec<u64> = findings_in(&v, "a.js").into_iter().filter(|(rule, _)| rule == "no-var").map(|(_, l)| l).collect();
        assert_eq!(lines, vec![2, 4], "{v}");
        let rules = rule_list(&v, "js");
        for r in ["no-var", "unicorn/prefer-at", "object-shorthand", "javascript-http-client-package"] {
            assert!(rules.iter().any(|x| x == r), "{r} in {rules:?}");
        }
    }
}

/// A7: `--range a...b` diffs b against merge-base(a, b): a change made on
/// `a` after the fork is not part of it; `a..b` stays a plain diff.
#[test]
fn range_with_three_dots_uses_the_merge_base() {
    let r = python_repo("three-dots", ">=3.12", "x = 1\n");
    git(&r.0, &["checkout", "-q", "-b", "topic"]);
    fs::write(r.0.join("topic.py"), OLD_JOIN).unwrap();
    git(&r.0, &["add", "-A"]);
    git(&r.0, &["commit", "-q", "--no-verify", "-m", "topic"]);
    git(&r.0, &["checkout", "-q", "main"]);
    fs::write(r.0.join("app.py"), OLD_JOIN).unwrap();
    git(&r.0, &["add", "-A"]);
    git(&r.0, &["commit", "-q", "--no-verify", "-m", "main moves on"]);
    let (_, v) = scan(&r.0, &["--range", "main...topic", "--stacks", "python"]);
    assert_eq!(stack_of(&v, "python")["files"], serde_json::json!(["topic.py"]), "{v}");
    assert_eq!((v["change"]["mode"].as_str(), v["change"]["merge_base"].as_bool()), (Some("range"), Some(true)));
    let (_, v) = scan(&r.0, &["--range", "main..topic", "--stacks", "python"]);
    assert_eq!(stack_of(&v, "python")["files"], serde_json::json!(["app.py", "topic.py"]), "{v}");
    // Unrelated histories: the hint names the revisions as typed, not their hashes.
    git(&r.0, &["checkout", "-q", "--orphan", "other"]);
    git(&r.0, &["commit", "-q", "--allow-empty", "--no-verify", "-m", "orphan"]);
    let out = Command::new(env!("CARGO_BIN_EXE_modern-tara")).args(["scan", "--quiet", "--range", "main...other", "--repo"]).arg(&r.0).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("no common ancestor of main and other (unrelated histories): use main..other"), "{stderr}");
}

/// A7: ~600 long paths run in chunks; one command line would be over the
/// Windows limit (os error 206).
#[test]
fn long_file_lists_are_chunked() {
    let deep = format!("{}/{}", "d".repeat(60), "e".repeat(60));
    let files: Vec<(String, &str)> = (0..600).map(|i| (format!("{deep}/module_with_a_long_name_{i:04}.py"), OLD_JOIN)).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), *c)).collect();
    let r = plain_repo("chunks", &refs);
    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "python"]);
    let report = stack_of(&v, "python");
    if report["status"] == "skipped" {
        return;
    }
    assert_eq!(report["status"], "ok", "{}", report["message"]);
    let found = v["findings"].as_array().unwrap().iter().filter(|f| f["rule"] == JOIN_RULE).count();
    assert_eq!(found, 600, "{}", report["message"]);
    assert_eq!(code, 1);
}

/// Modern CSS @eslint/css rejects as a syntax error (`@custom-variant`,
/// `@import ... source(none)`, CSS `if()`) does not make the file unscanned,
/// and the finding on the physical property is still reported.
#[test]
fn modern_css_syntax_is_scanned_not_unscanned() {
    let Some((_, v)) = check_fixture_doc("css-modern", "css") else { return };
    assert_eq!(unscanned(&v, "css"), vec![], "{v}");
}

/// `using module` of a module that is not installed is a parse error
/// `PSScriptAnalyzer` reports as a record, but a note, not an unscanned file:
/// the rest of the file is analysed.
#[test]
fn powershell_using_a_missing_module_is_scanned() {
    let Some((report, v)) = check_fixture_doc("powershell-using", "powershell") else { return };
    assert!(report["message"].as_str().unwrap_or_default().contains("ModuleNotFoundDuringParse"), "{v}");
    assert_eq!(unscanned(&v, "powershell"), vec![], "{v}");
}

/// `[SuppressMessage]` does not hide a finding: `PSScriptAnalyzer` runs with
/// `-IncludeSuppressed`.
#[test]
fn powershell_suppress_message_does_not_hide_findings() {
    check_fixture_doc("powershell-suppressed", "powershell");
}

/// A file ruff cannot read (Windows-1254 bytes, not UTF-8) is an unscanned
/// file, never a clean one and never an E902 finding.
#[test]
fn python_file_that_is_not_utf8_is_unscanned() {
    let r = python_repo("non-utf8", ">=3.12", "x = 1\n");
    fs::write(r.0.join("latin.py"), b"# \xfe\xfd\nimport os\nx = os.path.join('a', 'b')\n").unwrap();
    let (code, v) = scan(&r.0, &["--all-lines", "--stacks", "python", "--gate"]);
    if stack_of(&v, "python")["status"] != "ok" {
        eprintln!("python: {}", stack_of(&v, "python")["message"]);
        return;
    }
    let list = unscanned(&v, "python");
    assert_eq!(list.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), vec!["latin.py"], "{v}");
    assert!(list[0].1.contains("valid UTF-8"), "{list:?}");
    assert!(v["findings"].as_array().unwrap().iter().all(|f| f["rule"] != "E902"), "{v}");
    assert_eq!(code, 2, "{v}");
}

/// Inline comments do not hide CSS findings: `stylelint-disable` (stylelint
/// `ignoreDisables`) and `eslint-disable` (@eslint/css `noInlineConfig`).
#[test]
fn css_inline_disable_comments_do_not_hide_findings() {
    let r = plain_repo(
        "css-inline",
        &[(
            "a.css",
            "/* stylelint-disable color-function-notation */\na { color: rgba(0, 0, 0, 0.5); }\n/* eslint-disable css/prefer-logical-properties */\nb { margin-left: 1px; }\n",
        )],
    );
    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "css"]);
    if stack_of(&v, "css")["status"] != "ok" {
        return;
    }
    let found = findings_in(&v, "a.css");
    assert!(found.contains(&("color-function-notation".to_string(), 2)), "stylelint-disable hid it: {v}");
    assert!(found.contains(&("css/prefer-logical-properties".to_string(), 4)), "eslint-disable hid it: {v}");
}

/// A CSS file that does not parse is unscanned, never clean.
#[test]
fn css_that_does_not_parse_is_unscanned() {
    let r = plain_repo("css-broken", &[("broken.css", "a {\n  color: red;\n")]);
    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "css", "--gate"]);
    if stack_of(&v, "css")["status"] != "ok" {
        return;
    }
    let list = unscanned(&v, "css");
    assert_eq!(list.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), vec!["broken.css"], "{v}");
    assert!(list[0].1.contains("not parsed"), "{list:?}");
}

/// A file over 8 MiB is not copied: unscanned, in every stack that touched it.
#[test]
fn file_over_8_mib_is_unscanned() {
    let r = python_repo("big-file", ">=3.12", "x = 1\n");
    fs::write(r.0.join("big.py"), format!("x = '{}'\n", "a".repeat(9 << 20))).unwrap();
    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "python", "--gate"]);
    if stack_of(&v, "python")["status"] != "ok" {
        return;
    }
    let list = unscanned(&v, "python");
    assert_eq!(list.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), vec!["big.py"], "{v}");
    assert!(list[0].1.contains("over 8 MiB"), "{list:?}");
}

/// A `.go` file with no go.mod above it is unscanned.
#[test]
fn go_file_without_go_mod_is_unscanned() {
    let r = plain_repo("go-no-mod", &[("a.go", "package a\n")]);
    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "go", "--gate"]);
    if go_report(&v).is_none() {
        return;
    }
    assert_eq!(unscanned(&v, "go"), vec![("a.go".to_string(), "no go.mod above it: not analysed".to_string())], "{v}");
}

/// A package go cannot load (`//go:embed` of an untracked folder) leaves its
/// file unscanned with that reason; a package that fails to type-check under
/// every GOOS leaves its file unscanned as "no GOOS pass compiled it".
#[test]
fn go_unloadable_and_uncompilable_packages_are_unscanned() {
    let r = files_repo(
        "go-unscanned",
        &[("go.mod", "module example.com/u\n\ngo 1.21\n"), (".gitignore", "web/dist/\n")],
        &[
            ("web/web.go", "package web\n\nimport \"embed\"\n\n//go:embed dist\nvar FS embed.FS\n"),
            ("bad/bad.go", "package bad\n\nvar X int = \"s\"\n"),
            ("ok/ok.go", "package ok\n\nfunc F(v interface{}) {}\n"),
        ],
    );
    let (_, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go", "--gate"]);
    if go_report(&v).is_none() {
        return;
    }
    let list = unscanned(&v, "go");
    let reason = |f: &str| list.iter().find(|(file, _)| file == f).map(|(_, r)| r.clone());
    assert!(reason("web/web.go").is_some_and(|r| r.contains("go cannot load its package")), "{v}");
    assert_eq!(reason("bad/bad.go").as_deref(), Some("no GOOS pass compiled it"), "{v}");
    assert_eq!(reason("ok/ok.go"), None, "{v}");
}

/// A `.cs` file with no `.csproj` above it is unscanned (dotnet format never ran).
#[test]
fn csharp_file_without_csproj_is_unscanned() {
    let r = plain_repo("cs-no-proj", &[("a.cs", "class A { }\n")]);
    let (_, v) = scan(&r.0, &["--all-lines", "--stacks", "csharp", "--gate"]);
    if stack_of(&v, "csharp")["status"] == "skipped" {
        return;
    }
    let list = unscanned(&v, "csharp");
    assert_eq!(list.len(), 1, "{v}");
    assert!(list[0].0 == "a.cs" && list[0].1.contains("no .csproj above it"), "{list:?}");
}

/// A file `PSScriptAnalyzer` fails on (the runner lists it under `failed`) is
/// unscanned. The runner is replaced by a stub that fails every file.
#[test]
fn powershell_file_the_analyzer_failed_on_is_unscanned() {
    if installed("pwsh").is_none() {
        return;
    }
    let home = bare_home("pssa-failed", None);
    fs::create_dir_all(home.0.join("ps")).unwrap();
    let stub = "param($Job, $Out)\n$spec = Get-Content -LiteralPath $Job -Raw | ConvertFrom-Json\n$failed = @($spec.groups | ForEach-Object { $_.files })\n[ordered]@{ pssa = '1.0'; missing = $false; results = @(); errors = @('boom'); failed = $failed } | ConvertTo-Json -Depth 4 -Compress | Set-Content -LiteralPath $Out -Encoding utf8\n";
    fs::write(home.0.join("ps").join("run-pssa.ps1"), stub).unwrap();
    let r = plain_repo("pssa-failed", &[("a.ps1", "Write-Output 1\n")]);
    let (_, v, _) = scan_home(&r.0, &home.0, &["--all-lines", "--stacks", "powershell", "--gate"]);
    let report = stack_of(&v, "powershell");
    if report["status"] == "skipped" {
        return;
    }
    assert_eq!(unscanned(&v, "powershell"), vec![("a.ps1".to_string(), "PSScriptAnalyzer failed on it".to_string())], "{v}");
}

/// A file only another GOARCH includes (`*_arm64.go`, `linux && arm64`) is
/// analysed by a pass for that CPU type, never left unscanned.
#[test]
fn go_files_for_another_goarch_are_analysed() {
    let any = |pkg: &str| format!("package {pkg}\n\nfunc F(v interface{{}}) {{}}\n");
    let r = files_repo(
        "go-arch",
        &[("go.mod", "module example.com/a\n\ngo 1.21\n"), ("a.go", "package a\n")],
        &[("cpu/y_arm64.go", &any("cpu")), ("tagged/t.go", &format!("//go:build linux && arm64\n\n{}", any("tagged")))],
    );
    let (_, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go", "--gate"]);
    let Some(report) = go_report(&v) else { return };
    assert_eq!(report["status"], "ok", "{}", report["message"]);
    assert_eq!(unscanned(&v, "go"), vec![], "{v}");
    for file in ["cpu/y_arm64.go", "tagged/t.go"] {
        assert!(v["findings"].as_array().unwrap().iter().any(|f| f["file"] == file && f["rule"] == "go/any"), "{file}: {v}");
    }
}

/// Fallback files whose constraint only negates (`*_generic.go`, `*_other.go`)
/// are compiled by a GOOS/GOARCH they do not name.
#[test]
fn go_files_behind_negated_constraints_are_analysed() {
    let any = |pkg: &str| format!("package {pkg}\n\nfunc F(v interface{{}}) {{}}\n");
    let r = files_repo(
        "go-negated",
        &[("go.mod", "module example.com/a\n\ngo 1.21\n"), ("a.go", "package a\n")],
        &[
            ("generic/g.go", &format!("//go:build !amd64 && !arm64\n\n{}", any("generic"))),
            ("other/o.go", &format!("//go:build !windows && !linux && !darwin\n\n{}", any("other"))),
        ],
    );
    let (_, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go", "--gate"]);
    let Some(report) = go_report(&v) else { return };
    assert_eq!(report["status"], "ok", "{}", report["message"]);
    assert_eq!(unscanned(&v, "go"), vec![], "{v}");
    for file in ["generic/g.go", "other/o.go"] {
        assert!(v["findings"].as_array().unwrap().iter().any(|f| f["file"] == file && f["rule"] == "go/any"), "{file}: {v}");
    }
}

/// A GOOS with no amd64 port (`wasip1`, `js`) is vetted with a GOARCH it has
/// (wasm): two extra passes, both analysed, nothing unscanned.
#[test]
fn go_files_for_goos_without_amd64_are_analysed() {
    let any = |pkg: &str| format!("package {pkg}\n\nfunc F(v interface{{}}) {{}}\n");
    let r = files_repo(
        "go-wasm",
        &[("go.mod", "module example.com/w\n\ngo 1.21\n"), ("a.go", "package w\n")],
        &[("web/z_wasip1.go", &any("web")), ("x_js.go", &any("w"))],
    );
    let (_, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go", "--gate"]);
    let Some(report) = go_report(&v) else { return };
    assert_eq!(report["status"], "ok", "{}", report["message"]);
    assert_eq!(unscanned(&v, "go"), vec![], "{v}");
    for file in ["web/z_wasip1.go", "x_js.go"] {
        assert!(v["findings"].as_array().unwrap().iter().any(|f| f["file"] == file && f["rule"] == "go/any"), "{file}: {v}");
    }
}

/// Files beyond the three-pass cap stay candidates until every pass has run:
/// one a later pass compiles is analysed, not listed unscanned; only a file
/// still uncovered carries the cap reason.
#[test]
fn go_over_cap_files_that_a_later_pass_compiles_are_not_unscanned() {
    let file = |tag: &str, name: &str| (format!("{name}.go"), format!("//go:build {tag}\n\npackage t\n\nfunc F{name}(v interface{{}}) {{}}\n"));
    let files = [file("z", "z1"), file("z", "z2"), file("ta", "ta"), file("tb", "tb"), file("tc", "tc"), file("ua || z", "f")];
    let change: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
    let r = files_repo("go-cap-late", &[("go.mod", "module example.com/c\n\ngo 1.21\n"), ("a.go", "package t\n")], &change);
    let (_, v) = scan(&r.0, &["--range", "HEAD~1..HEAD", "--stacks", "go", "--gate"]);
    let Some(report) = go_report(&v) else { return };
    assert_eq!(report["status"], "ok", "{}", report["message"]);
    let list = unscanned(&v, "go");
    assert_eq!(list.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), vec!["tc.go"], "{v}");
    assert!(list[0].1.contains("more than 3 extra"), "{list:?}");
    assert!(v["findings"].as_array().unwrap().iter().any(|f| f["file"] == "f.go" && f["rule"] == "go/any"), "f.go is compiled by the z pass: {v}");
}
