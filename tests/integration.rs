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

/// A one-commit Python repository: `pyproject.toml` with `requires` and `app.py`.
fn python_repo(name: &str, requires: &str, code: &str) -> TempRepo {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mt-test-{name}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let guard = TempRepo(dir.clone());
    fs::write(dir.join("pyproject.toml"), format!("[project]\nname = \"gate\"\nversion = \"0\"\nrequires-python = \"{requires}\"\n")).unwrap();
    fs::write(dir.join("app.py"), code).unwrap();
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

#[test]
fn csharp_fixture() {
    check("csharp");
}

#[test]
fn js_and_ts_fixture() {
    check("js");
}

#[test]
fn css_fixture() {
    check("css");
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
            if ["LICENSE", "Cargo.lock", "package-lock.json"].contains(&name.as_str()) {
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
