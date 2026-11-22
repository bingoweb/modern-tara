//! Go: the `go fix` modernizers (cmd/fix, Go 1.26+) through
//! `go vet -vettool=<GOTOOLDIR>/fix -json` on a copy of each module, over the
//! packages that hold changed files: first for the host GOOS, then linux,
//! windows and darwin for changed files the host build leaves out. A second
//! copy with the go.mod `go` line raised to the installed toolchain finds what
//! the pin holds back (`needs-newer`); the minors in between say which
//! version each finding needs. All go calls share one time budget.

use super::{Ctx, astgrep};
use crate::change::{self, Change};
use crate::pins::{self, GoVersion, NEWEST_GO};
use crate::proc::{Cmd, Output};
use crate::report::{Confidence, Finding, Kind, StackReport, Upgrade};
use serde_json::Value;
use std::cell::{Cell, OnceCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The cmd/fix modernizers of go1.27.4; the catalogue
/// `~/.claude/agents/modern/go.md` documents each. An analyzer missing here
/// (a newer Go added it) gives low-confidence findings until it is catalogued.
pub const ANALYZERS: [&str; 24] = [
    "any",
    "atomictypes",
    "embedlit",
    "errorsastype",
    "forvar",
    "inline",
    "mapsloop",
    "minmax",
    "newexpr",
    "omitzero",
    "plusbuild",
    "rangeint",
    "reflecttypefor",
    "slicesbackward",
    "slicescontains",
    "slicessort",
    "stditerators",
    "stringsbuilder",
    "stringscut",
    "stringscutprefix",
    "stringsseq",
    "testingcontext",
    "unsafefuncs",
    "waitgroupgo",
];

/// Correctness checks in cmd/fix, not modernizers: off with `-NAME=false`.
pub const DISABLED: [&str; 2] = ["buildtag", "hostport"];

/// Modernizers whose fix changes behaviour: `omitzero` changes encoding/json output.
pub const LOW: [&str; 1] = ["omitzero"];

/// The first Go whose `go fix` runs the modernizers.
const MIN_GO: (u32, u32) = (1, 26);

/// GOOS passes after the host's, for changed files its build leaves out.
const OTHER_GOOS: [&str; 3] = ["linux", "windows", "darwin"];

/// GOARCH of the cross passes: every GOOS above has it.
const CROSS_GOARCH: &str = "amd64";

/// The upgrade pin of Go findings.
const PIN: &str = "go.mod go";

/// The stack's whole time budget unless `--timeout` sets one.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// On every go call: no toolchain download, module mode, and none of the
/// user's GOFLAGS (an empty GOFLAGS falls back to `go env -w GOFLAGS`, so a
/// flag that changes nothing for vet and list takes its place). GOWORK,
/// GOOS and GOARCH are set per call.
pub const GO_ENV: [(&str, &str); 4] = [("GOTOOLCHAIN", "local"), ("GOFLAGS", "-buildvcs=false"), ("GO111MODULE", "on"), ("NO_COLOR", "1")];

const TIMED_OUT: &str = "timed out";

type Key3 = (String, u32, String);
type Key5 = (String, u32, u32, String, String);

pub fn confidence(analyzer: &str) -> Confidence {
    if ANALYZERS.contains(&analyzer) && !LOW.contains(&analyzer) { Confidence::High } else { Confidence::Low }
}

/// One deadline for all go calls of the stack.
struct Budget {
    deadline: Instant,
    total: Duration,
    spent: Cell<bool>,
}

impl Budget {
    fn new(total: Duration) -> Budget {
        Budget { deadline: Instant::now() + total, total, spent: Cell::new(false) }
    }
    /// What is left; `None`, and spent from then on, once the deadline passed.
    fn left(&self) -> Option<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || self.spent.get() {
            self.spent.set(true);
            None
        } else {
            Some(left)
        }
    }
    fn spend(&self) {
        self.spent.set(true);
    }
    fn spent(&self) -> bool {
        self.spent.get()
    }
}

/// The installed toolchain.
struct Go {
    exe: PathBuf,
    fix: PathBuf,
    version: GoVersion,
    /// `go1.27.4`.
    version_text: String,
    host_os: String,
    host_arch: String,
    /// `-buildtag=false -hostport=false`, for the flags this fix tool has.
    off: Vec<String>,
    budget: Budget,
    /// `go list std`, read once when first needed; `None` when it failed.
    std: OnceCell<Option<BTreeSet<String>>>,
}

/// Where the go calls of one copy run.
struct Site {
    copy: PathBuf,
    cwd: PathBuf,
    /// The copied go.work, when the module is in a workspace.
    gowork: Option<PathBuf>,
}

fn with_env(mut cmd: Cmd) -> Cmd {
    for (k, v) in GO_ENV {
        cmd = cmd.env(k, v);
    }
    cmd
}

impl Go {
    /// A go call with GOOS, GOARCH and GOWORK pinned and the budget's rest as timeout.
    fn cmd(&self, site: &Site, goos: &str) -> Result<Cmd, String> {
        let left = self.budget.left().ok_or_else(|| TIMED_OUT.to_string())?;
        let arch = if goos == self.host_os { self.host_arch.as_str() } else { CROSS_GOARCH };
        let mut cmd = with_env(Cmd::new(&self.exe)).cwd(&site.cwd).env("GOOS", goos).env("GOARCH", arch).timeout(left);
        cmd = match &site.gowork {
            Some(w) => cmd.env("GOWORK", w),
            None => cmd.env("GOWORK", "off"),
        };
        if goos != self.host_os {
            // A cross build has no C compiler for the target.
            cmd = cmd.env("CGO_ENABLED", "0");
        }
        Ok(cmd)
    }
    fn run(&self, cmd: Cmd) -> Result<Output, String> {
        let out = cmd.run()?;
        if out.timed_out {
            self.budget.spend();
            return Err(TIMED_OUT.into());
        }
        Ok(out)
    }
    /// Whether `path` is a standard-library package: `go list std` for the
    /// host and for GOOS=linux together, once per scan; without either, a path
    /// whose first element has no dot.
    fn is_std(&self, path: &str) -> bool {
        let std = self.std.get_or_init(|| {
            let list = |goos: &str, arch: &str| -> Option<BTreeSet<String>> {
                let left = self.budget.left()?;
                let out = with_env(Cmd::new(&self.exe))
                    .env("GOWORK", "off")
                    .env("GOOS", goos)
                    .env("GOARCH", arch)
                    .env("CGO_ENABLED", "0")
                    .args(["list", "std"])
                    .cwd(&std::env::temp_dir())
                    .timeout(left.min(Duration::from_secs(60)))
                    .run()
                    .ok()?;
                out.ok().then(|| out.stdout_text().lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
            };
            let host = list(&self.host_os, &self.host_arch);
            let linux = if self.host_os == "linux" { None } else { list("linux", CROSS_GOARCH) };
            match (host, linux) {
                (Some(mut a), Some(b)) => {
                    a.extend(b);
                    Some(a)
                }
                (a, b) => a.or(b),
            }
        });
        std.as_ref().map_or_else(|| dotless(path), |set| set.contains(path))
    }
}

/// `go1.27.4`, `go1.27.4 X:boringcrypto`, `devel go1.28-abcdef ...`.
fn installed_version(text: &str) -> Option<GoVersion> {
    let first = text.split_whitespace().next().unwrap_or("");
    pins::go_version(first).or_else(|| {
        let at = text.find("go1.")?;
        let v: String = text[at + 2..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        pins::go_version(v.trim_end_matches('.'))
    })
}

/// `GOTOOLDIR/fix` with the host's executable suffix: `go env GOEXE`
/// follows GOOS, so a user GOOS=linux would drop the `.exe`.
fn fix_path(tooldir: &str) -> PathBuf {
    Path::new(tooldir).join(format!("fix{}", std::env::consts::EXE_SUFFIX))
}

/// `-NAME=false` for each correctness check the fix tool lists.
fn disable_flags(names: &[String]) -> Vec<String> {
    DISABLED.iter().filter(|d| names.iter().any(|n| n == *d)).map(|d| format!("-{d}=false")).collect()
}

/// The arguments of one `go vet` run.
fn vet_args(fix: &Path, off: &[String], mod_mod: bool, patterns: &[String]) -> Vec<String> {
    let mut args = vec!["vet".to_string()];
    if mod_mod {
        args.push("-mod=mod".into());
    }
    args.push(format!("-vettool={}", fix.display()));
    args.push("-json".into());
    args.extend(off.iter().cloned());
    args.extend(patterns.iter().cloned());
    args
}

/// The toolchain, or the reason to skip the stack.
fn probe(exe: &Path, budget: Budget) -> Result<Go, String> {
    let out = with_env(Cmd::new(exe))
        .env("GOWORK", "off")
        .env_remove("GOOS")
        .env_remove("GOARCH")
        .args(["env", "-json", "GOVERSION", "GOTOOLDIR", "GOHOSTOS", "GOHOSTARCH"])
        .cwd(&std::env::temp_dir())
        .timeout(Duration::from_secs(60))
        .run()?;
    if !out.ok() {
        return Err(format!("`go env` failed: {}", out.tail(2)));
    }
    let env: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("`go env -json`: {e}"))?;
    let text = |k: &str| env.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let version_text = text("GOVERSION");
    let version = installed_version(&version_text).ok_or_else(|| format!("unreadable Go version `{version_text}`"))?;
    if (version.major, version.minor) < MIN_GO {
        return Err(format!("{version_text} has no `go fix` modernizers: install Go {}.{} or newer.", MIN_GO.0, MIN_GO.1));
    }
    let fix = fix_path(&text("GOTOOLDIR"));
    if !fix.is_file() {
        return Err(format!("{version_text}: {} is missing.", fix.display()));
    }
    let flags = Cmd::new(&fix).arg("-flags").cwd(&std::env::temp_dir()).timeout(Duration::from_secs(60)).run()?;
    let names: Vec<String> = serde_json::from_slice::<Value>(&flags.stdout)
        .ok()
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|f| f.get("Name").and_then(Value::as_str).map(String::from)).collect()))
        .unwrap_or_default();
    if !names.iter().any(|n| n == "json") {
        return Err(format!("{version_text}: `{} -flags` lists no -json flag.", fix.display()));
    }
    Ok(Go {
        exe: exe.to_path_buf(),
        fix,
        version,
        version_text,
        host_os: text("GOHOSTOS"),
        host_arch: text("GOHOSTARCH"),
        off: disable_flags(&names),
        budget,
        std: OnceCell::new(),
    })
}

/// A go.work that lists a module in `use`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Workspace {
    /// Repo-relative go.work.
    file: String,
    /// Repo-relative module folders of its `use` lines.
    uses: Vec<String>,
}

struct Module {
    /// Repo-relative folder of the go.mod ("" = root).
    root: String,
    go_mod: String,
    /// Changed .go files in it.
    files: Vec<String>,
    /// The changed files the go passes analyse: not under `vendor/` or
    /// `testdata/`, no `_` or `.` names, no `//go:build ignore`.
    wanted: BTreeSet<String>,
    /// The other changed files.
    ignored: Vec<String>,
    /// The `go` directive as written; `None` when the line is missing.
    go_text: Option<String>,
    /// `None` when the directive is unreadable.
    version: Option<GoVersion>,
    workspace: Option<Workspace>,
}

impl Module {
    fn label(&self) -> &str {
        if self.root.is_empty() { "." } else { &self.root }
    }
    /// The `go` pin for `upgrade.from` and catalogue findings.
    fn pin_value(&self) -> String {
        self.go_text.clone().unwrap_or_else(|| "1.16 (no go line)".into())
    }
    fn pin_label(&self) -> String {
        match &self.go_text {
            None => format!("1.16 ({}: no go line, which Go reads as 1.16)", self.go_mod),
            Some(t) if self.version.is_none() => format!("{t} ({}, unreadable)", self.go_mod),
            Some(t) => format!("{t} ({})", self.go_mod),
        }
    }
    /// The module path of `repo_dir` as a package pattern (`.`, `./board`).
    fn pattern(&self, repo_dir: &str) -> String {
        let rel = if self.root.is_empty() { repo_dir } else { repo_dir.strip_prefix(&format!("{}/", self.root)).unwrap_or("") };
        if rel.is_empty() { ".".into() } else { format!("./{rel}") }
    }
}

pub fn run(ctx: &Ctx, files: &[String]) -> StackReport {
    let mut report = StackReport::new("go", files.to_vec());
    report.newest("go", NEWEST_GO);
    let Some(exe) = ctx.tools.go.clone() else {
        report.skip("go not found: install Go 1.26 or newer (cmd/fix ships with the toolchain).");
        return report;
    };
    let go = match probe(&exe, Budget::new(ctx.timeout_or(DEFAULT_TIMEOUT_SECS))) {
        Ok(g) => g,
        Err(e) => {
            report.skip(e);
            return report;
        }
    };
    report.tool = format!("{} cmd/fix", go.version_text);
    let newest_pin = pins::go_version(NEWEST_GO).unwrap_or(go.version);
    let newest = if (go.version.major, go.version.minor) > (newest_pin.major, newest_pin.minor) { go.version.minor_text() } else { NEWEST_GO.to_string() };
    report.newest("go", newest.clone());
    if (go.version.major, go.version.minor) < (newest_pin.major, newest_pin.minor) {
        report.note(format!("{} is older than the newest stable Go {NEWEST_GO}: findings that need a newer go line are not visible.", go.version_text));
    }

    let (modules, loose) = locate(ctx.change, files);
    let mut pin_json = serde_json::Map::new();
    for m in &modules {
        for f in &m.files {
            pin_json.insert(f.clone(), Value::String(m.pin_label()));
        }
    }
    for f in &loose {
        pin_json.insert(f.clone(), Value::String("no go.mod".into()));
    }
    report.pin("go", Value::Object(pin_json));
    if !loose.is_empty() {
        report.note(format!("no go.mod above {} file(s); not analysed.", loose.len()));
    }

    let catalogue = catalogue(ctx, &mut report, &modules, &newest);
    let mut not_done: Vec<String> = Vec::new();
    for (i, m) in modules.iter().enumerate() {
        if go.budget.spent() {
            if !m.wanted.is_empty() {
                not_done.push(short_list(&m.wanted.iter().cloned().collect::<Vec<_>>()));
            }
            continue;
        }
        if let Some(what) = run_module(ctx, &mut report, &go, m, i, &newest, &catalogue) {
            not_done.push(what);
        }
    }
    if go.budget.spent() {
        report.fail(format!("timed out after {}s (raise --timeout); not analysed: {}", go.budget.total.as_secs(), not_done.join(", ")));
    }
    let unknown: BTreeSet<&str> = report.findings.iter().filter_map(|f| f.rule.strip_prefix("go/")).filter(|a| !ANALYZERS.contains(a)).collect();
    if !unknown.is_empty() {
        let names = unknown.into_iter().collect::<Vec<_>>().join(", ");
        report.note(format!("go fix analyzer(s) not catalogued yet, so low confidence: {names}"));
    }
    report
}

/// Catalogue rules for the std calls the inliner migrates but refuses below
/// the toolchain's go line: an `inline` finding on their line is dropped.
pub const INLINE_RULES: [&str; 4] = ["go-ioutil-io-os", "go-reflect-ptr-pointer", "go-importer-for", "go-types-newsignature"];

/// Catalogue rules (rules/go.yml) over the files the go passes analyse;
/// returns the lines an `INLINE_RULES` rule matched.
fn catalogue(ctx: &Ctx, report: &mut StackReport, modules: &[Module], newest: &str) -> HashSet<(String, u32)> {
    let mut lines = HashSet::new();
    let files: Vec<String> = modules.iter().flat_map(|m| m.wanted.iter().cloned()).collect();
    if files.is_empty() || !ctx.home.join("rules").join("go.yml").is_file() {
        return lines;
    }
    let Ok(copy) = ctx.temp.sub("go-catalogue") else { return lines };
    let _ = ctx.change.materialize(&copy, &files);
    match astgrep::scan(ctx, &copy, &files) {
        Ok(Some(matches)) => {
            for m in matches.into_iter().filter(|m| m.gate == "go") {
                let module = modules.iter().find(|md| md.wanted.contains(&m.file));
                let have = module.and_then(|md| md.version).map(|v| (v.major, v.minor));
                let below = matches!((have, pins::go_version(&m.min)), (Some(h), Some(need)) if h < (need.major, need.minor));
                let note = format!("newest stable Go is {newest}");
                let pin = module.map(Module::pin_value);
                let mut f = astgrep::to_finding("go", m, pin.as_deref(), below, &note);
                if let Some(u) = &mut f.upgrade {
                    u.pin = PIN.into();
                }
                if INLINE_RULES.contains(&f.rule.as_str()) {
                    lines.insert((f.file.clone(), f.line));
                }
                report.findings.push(f);
            }
        }
        Ok(None) => report.note("ast-grep not found: catalogue rules skipped."),
        Err(e) => report.note(e),
    }
    lines
}

/// Modules (nearest go.mod) holding the changed files, and files with none above.
fn locate(change: &Change, files: &[String]) -> (Vec<Module>, Vec<String>) {
    let mut by_root: BTreeMap<String, Module> = BTreeMap::new();
    let mut loose = Vec::new();
    for f in files {
        let Some(go_mod) = change::ancestors(change::parent(f)).into_iter().map(|d| change::join(&d, "go.mod")).find(|m| change.exists(m)) else {
            loose.push(f.clone());
            continue;
        };
        let root = change::parent(&go_mod).to_string();
        let m = by_root.entry(root.clone()).or_insert_with(|| {
            let parsed = pins::go_mod(&change.read_text(&go_mod).unwrap_or_default());
            let version = match &parsed.go {
                None => Some(GoVersion::lang(pins::GO_MOD_DEFAULT.0, pins::GO_MOD_DEFAULT.1)),
                Some(t) => pins::go_version(t),
            };
            let workspace = workspace_for(change, &root);
            Module { root, go_mod, files: Vec::new(), wanted: BTreeSet::new(), ignored: Vec::new(), go_text: parsed.go, version, workspace }
        });
        m.files.push(f.clone());
    }
    let mut modules: Vec<Module> = by_root.into_values().collect();
    for m in &mut modules {
        let texts = change.read(&m.files).unwrap_or_default();
        for f in &m.files {
            let rel = if m.root.is_empty() { f.as_str() } else { f.trim_prefix(format!("{}/", m.root).as_str()) };
            let tag = texts.get(f).and_then(|b| pins::go_build_constraint(&String::from_utf8_lossy(b)));
            if skipped_by_go(rel) || tag.as_deref() == Some("ignore") {
                m.ignored.push(f.clone());
            } else {
                m.wanted.insert(f.clone());
            }
        }
    }
    (modules, loose)
}

/// The nearest go.work at or above `root`, when its `use` lines list `root`
/// (as Go does: the nearest go.work decides).
fn workspace_for(change: &Change, root: &str) -> Option<Workspace> {
    for dir in change::ancestors(root) {
        let file = change::join(&dir, "go.work");
        if !change.exists(&file) {
            continue;
        }
        let text = change.read_text(&file).unwrap_or_default();
        let uses: Vec<String> = pins::go_work_uses(&text).iter().filter_map(|u| change::normalize(&change::join(&dir, u))).collect();
        return uses.iter().any(|u| u == root).then_some(Workspace { file, uses });
    }
    None
}

/// Every file the module's copy needs: its own set, and for a workspace the
/// go.work, go.work.sum and each `use` module (plus go.work `replace` folders).
fn sources(change: &Change, m: &Module) -> Vec<String> {
    let mut out: BTreeSet<String> = copy_set(change, &m.root).into_iter().collect();
    if let Some(ws) = &m.workspace {
        let dir = change::parent(&ws.file);
        out.insert(ws.file.clone());
        let sum = change::join(dir, "go.work.sum");
        if change.exists(&sum) {
            out.insert(sum);
        }
        let mut roots = ws.uses.clone();
        for target in pins::go_mod_replaces(&change.read_text(&ws.file).unwrap_or_default()) {
            if let Some(norm) = change::normalize(&change::join(dir, &target)) {
                roots.push(norm);
            }
        }
        for r in roots.iter().filter(|r| change.exists(&change::join(r, "go.mod"))) {
            out.extend(copy_set(change, r));
        }
    }
    out.into_iter().collect()
}

/// Files to copy for the module at `root`: go.mod, go.sum, `vendor/`, and
/// every file in a folder that holds a .go file or below such a folder
/// (`//go:embed` cannot reach upward). Nested modules stay out; folders of
/// local `replace` directives are added the same way.
fn copy_set(change: &Change, root: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    let mut done = BTreeSet::new();
    let mut queue = vec![root.to_string()];
    while let Some(dir) = queue.pop() {
        if !done.insert(dir.clone()) {
            continue;
        }
        out.extend(module_files(change, &dir));
        let go_mod = change::join(&dir, "go.mod");
        let text = change.read_text(&go_mod).unwrap_or_default();
        for target in pins::go_mod_replaces(&text) {
            if let Some(norm) = change::normalize(&change::join(&dir, &target))
                && change.exists(&change::join(&norm, "go.mod"))
            {
                queue.push(norm);
            }
        }
    }
    out.into_iter().collect()
}

fn module_files(change: &Change, root: &str) -> Vec<String> {
    let all = change.under(root, |_| true);
    let rel = |p: &str| -> String { if root.is_empty() { p.to_string() } else { p.trim_prefix(format!("{root}/").as_str()).to_string() } };
    let vendored = |p: &str| rel(p).starts_with("vendor/");
    let nested: Vec<String> = all
        .iter()
        .filter(|p| change::file_name(p) == "go.mod" && change::parent(p) != root && !vendored(p))
        .map(|p| format!("{}/", change::parent(p)))
        .collect();
    let in_nested = |p: &str| nested.iter().any(|d| p.starts_with(d.as_str()));
    let go_dirs: BTreeSet<&str> = all.iter().filter(|p| change::extension(p) == "go" && !in_nested(p)).map(|p| change::parent(p)).collect();
    all.iter()
        .filter(|p| !in_nested(p))
        .filter(|p| {
            let r = rel(p);
            r == "go.mod" || r == "go.sum" || r.starts_with("vendor/") || change::ancestors(change::parent(p)).iter().any(|d| go_dirs.contains(d.as_str()))
        })
        .cloned()
        .collect()
}

/// Whether the go tool skips this module-relative path in `./...`:
/// `testdata`, `vendor` and names starting with `_` or `.`.
fn skipped_by_go(rel: &str) -> bool {
    rel.starts_with("vendor/")
        || rel.split('/').enumerate().any(|(i, seg)| seg.starts_with(['_', '.']) || (seg == "testdata" && i + 1 < rel.split('/').count()))
}

/// One finding of the fix tool, repo-relative.
#[derive(Debug, Clone)]
struct Hit {
    file: String,
    line: u32,
    column: u32,
    analyzer: String,
    message: String,
    /// The GOOS of the pass that found it.
    goos: String,
}

impl Hit {
    /// What a report keeps once per line and rule.
    fn key3(&self) -> Key3 {
        (self.file.clone(), self.line, self.analyzer.clone())
    }
    /// What tells a raised-only diagnostic from a base one.
    fn key5(&self) -> Key5 {
        (self.file.clone(), self.line, self.column, self.analyzer.clone(), self.message.clone())
    }
    fn finding(&self, kind: Kind, upgrade: Option<Upgrade>) -> Finding {
        Finding {
            stack: "go",
            file: self.file.clone(),
            line: self.line,
            column: self.column,
            rule: format!("go/{}", self.analyzer),
            message: self.message.clone(),
            kind,
            source: "tool",
            confidence: confidence(&self.analyzer),
            upgrade,
        }
    }
    /// An `inline` finding on a line a catalogue rule already reports (the
    /// std calls whose catalogue rule carries the real minimum Go).
    fn catalogued(&self, catalogue: &HashSet<(String, u32)>) -> bool {
        self.analyzer == "inline" && catalogue.contains(&(self.file.clone(), self.line))
    }
}

/// The base pass's findings: once per file, line and rule, without
/// `inline` findings on catalogue lines.
fn base_findings(hits: &[Hit], catalogue: &HashSet<(String, u32)>) -> Vec<Finding> {
    let mut seen = HashSet::new();
    hits.iter().filter(|h| !h.catalogued(catalogue) && seen.insert(h.key3())).map(|h| h.finding(Kind::Modern, None)).collect()
}

/// Raised-copy hits that the base did not show (compared by file, line,
/// column, analyzer and message), once per file, line and rule; `inline`
/// on catalogue lines is left out. Also maps each raised diagnostic to its entry.
fn raised_only(base: &[Hit], raised: Vec<Hit>, wanted: &BTreeSet<String>, catalogue: &HashSet<(String, u32)>) -> (BTreeMap<Key3, Hit>, HashMap<Key5, Key3>) {
    let base: HashSet<Key5> = base.iter().map(Hit::key5).collect();
    let mut extra = BTreeMap::new();
    let mut by_key = HashMap::new();
    for h in raised {
        if !wanted.contains(&h.file) || base.contains(&h.key5()) || h.catalogued(catalogue) {
            continue;
        }
        by_key.insert(h.key5(), h.key3());
        extra.entry(h.key3()).or_insert(h);
    }
    (extra, by_key)
}

/// A diagnostic as printed, before it is mapped into the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Diag {
    path: String,
    line: u32,
    column: u32,
    analyzer: String,
    message: String,
    has_fix: bool,
}

#[derive(Debug, Default)]
struct VetJson {
    diags: Vec<Diag>,
    /// `<package> <analyzer>: <error>` for analyzers that reported an error.
    errors: Vec<String>,
    /// Packages with an entry (`pkg`, test variants folded in).
    packages: BTreeSet<String>,
    /// Top-level JSON objects read (`{}` included).
    objects: usize,
    /// Why parsing stopped early, if it did.
    broken: Option<String>,
}

/// `path:line:col` (or `path.go:line`), read from the right so a Windows
/// drive colon stays in the path.
fn parse_posn(posn: &str) -> Option<(String, u32, u32)> {
    let num = |s: &str| s.parse::<u32>().ok();
    let (rest, last) = posn.rsplit_once(':')?;
    let last = num(last)?;
    if let Some((path, line)) = rest.rsplit_once(':')
        && let Some(line) = num(line)
        && !path.is_empty()
    {
        return Some((path.to_string(), line, last));
    }
    // Without a column only a .go path is believable (`C:\a.go:12`, not `a.go:x:1`).
    Path::new(rest).extension().is_some_and(|e| e == "go").then(|| (rest.to_string(), last, 0))
}

/// `pkg`, `pkg [pkg.test]` and `pkg_test [pkg.test]` all name `pkg`.
fn package_base(id: &str) -> String {
    id.split_whitespace().next().unwrap_or(id).trim_suffix("_test").to_string()
}

/// The concatenated JSON objects `go vet -json` prints: `{}` for a clean
/// package, else `{"<package>": {"<analyzer>": [diagnostics] | {"error": "..."}}}`.
/// Package keys may carry a test variant (`pkg [pkg.test]`); only `posn` counts.
fn parse_vet(stdout: &[u8]) -> VetJson {
    let mut out = VetJson::default();
    for item in serde_json::Deserializer::from_slice(stdout).into_iter::<Value>() {
        let v = match item {
            Ok(v) => v,
            Err(e) => {
                out.broken = Some(e.to_string());
                break;
            }
        };
        out.objects += 1;
        let Some(packages) = v.as_object() else { continue };
        for (package, analyzers) in packages {
            out.packages.insert(package_base(package));
            let Some(analyzers) = analyzers.as_object() else { continue };
            for (analyzer, result) in analyzers {
                if let Some(err) = result.get("error") {
                    let text = err.as_str().map_or_else(|| err.to_string(), String::from);
                    out.errors.push(format!("{package} {analyzer}: {}", text.chars().take(200).collect::<String>()));
                    continue;
                }
                for d in result.as_array().into_iter().flatten() {
                    let Some((path, line, column)) = d.get("posn").and_then(Value::as_str).and_then(parse_posn) else { continue };
                    out.diags.push(Diag {
                        path,
                        line,
                        column,
                        analyzer: analyzer.clone(),
                        message: d.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
                        has_fix: d.get("suggested_fixes").and_then(Value::as_array).is_some_and(|f| !f.is_empty()),
                    });
                }
            }
        }
    }
    out
}

/// What a `# <package>` header on stderr failed: the package, or only its
/// test variant (`# pkg [pkg.test]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failed {
    Package,
    Tests,
}

/// Packages vet could not analyse; one with an entry in the JSON counts as analysed.
fn failed_packages(stderr: &str, analysed: &BTreeSet<String>) -> BTreeMap<String, Failed> {
    let mut out = BTreeMap::new();
    for rest in stderr.lines().filter_map(|l| l.strip_prefix("# ")) {
        let mut words = rest.split_whitespace();
        let Some(id) = words.next() else { continue };
        let base = package_base(id);
        if analysed.contains(&base) {
            continue;
        }
        let scope = if words.next().is_some_and(|w| w.starts_with('[')) || id.ends_with("_test") { Failed::Tests } else { Failed::Package };
        let entry = out.entry(base).or_insert(scope);
        if scope == Failed::Package {
            *entry = Failed::Package;
        }
    }
    out
}

/// An inliner refusal on a changed line: `cannot inline call to <callee>
/// (declared using <declared>) into a file using goX`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    file: String,
    line: u32,
    callee: String,
    declared: String,
}

/// `(callee, declared)` of the inliner's refusal to inline a newer callee into an older file.
fn parse_refusal(message: &str) -> Option<(String, String)> {
    let rest = message.strip_prefix("cannot inline call to ")?;
    let (callee, rest) = rest.split_once(" (declared using ")?;
    let (declared, rest) = rest.split_once(')')?;
    rest.trim_start().starts_with("into a file using go").then(|| (callee.to_string(), declared.to_string()))
}

/// The callee of `Call of <callee> should be inlined`.
fn inline_callee(message: &str) -> Option<&str> {
    message.strip_prefix("Call of ")?.strip_suffix(" should be inlined")
}

/// The package qualifier of `ioutil.ReadAll` or `(*bytes.Buffer).M`; `None` for a local `Old`.
fn qualifier(callee: &str) -> Option<&str> {
    let (q, _) = callee.trim_start_matches(['(', '*']).split_once('.')?;
    (!q.is_empty()).then_some(q)
}

/// The import path that `qualifier` names in the Go file `source`.
fn import_path_of(source: &str, qualifier: &str) -> Option<String> {
    let mut in_block = false;
    for line in source.lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        let spec = if in_block {
            if code.starts_with(')') {
                in_block = false;
                continue;
            }
            code
        } else if let Some(rest) = code.strip_prefix("import").filter(|r| r.starts_with([' ', '\t', '(', '"'])) {
            let rest = rest.trim_start();
            match rest.strip_prefix('(') {
                Some(inner) => {
                    in_block = true;
                    inner.trim()
                }
                None => rest,
            }
        } else if ["func ", "var ", "type ", "const "].iter().any(|k| code.starts_with(k)) {
            break;
        } else {
            continue;
        };
        let Some(open) = spec.find('"') else { continue };
        let Some(len) = spec[open + 1..].find('"') else { continue };
        let path = &spec[open + 1..open + 1 + len];
        let alias = spec[..open].trim();
        if alias == "_" {
            continue;
        }
        let mut parts: Vec<&str> = path.split('/').collect();
        if parts.len() > 1 && parts.last().is_some_and(|p| p.len() > 1 && p.starts_with('v') && p[1..].bytes().all(|b| b.is_ascii_digit())) {
            parts.pop();
        }
        let name = if alias.is_empty() || alias == "." { parts.last().copied().unwrap_or(path) } else { alias };
        if name == qualifier {
            return Some(path.to_string());
        }
    }
    None
}

/// The fallback std test when `go list std` failed: no dot in the first path element.
fn dotless(path: &str) -> bool {
    !path.split('/').next().unwrap_or("").contains('.')
}

/// Raised-only `inline` findings whose base pass refused the same call only
/// because the callee is a std function declared with the toolchain's Go:
/// taken out of `extra` (they are `modern`, never stepped).
fn std_inlines(
    extra: &mut BTreeMap<Key3, Hit>,
    refusals: &[Refusal],
    toolchain: GoVersion,
    source: &dyn Fn(&str) -> Option<String>,
    is_std: &dyn Fn(&str) -> bool,
) -> Vec<Hit> {
    let std_refused = |h: &Hit| {
        let Some(callee) = inline_callee(&h.message) else { return false };
        refusals.iter().any(|r| {
            r.file == h.file
                && r.line == h.line
                && r.callee == callee
                && pins::go_version(&r.declared).is_some_and(|v| (v.major, v.minor) == (toolchain.major, toolchain.minor))
                && qualifier(callee).and_then(|q| import_path_of(&source(&h.file)?, q)).is_some_and(|p| is_std(&p))
        })
    };
    let keys: Vec<Key3> = extra.iter().filter(|(_, h)| h.analyzer == "inline" && std_refused(h)).map(|(k, _)| k.clone()).collect();
    keys.into_iter().filter_map(|k| extra.remove(&k)).collect()
}

/// A "what to do" for go's common load failures.
fn hint(stderr: &str) -> Option<&'static str> {
    let has = |s: &str| stderr.contains(s);
    if has("no matching files found") || has("cannot embed") {
        Some("a //go:embed pattern points at files git does not track; build them before the scan")
    } else if has("updates to go.mod needed") || has("missing go.sum entry") {
        Some("go.mod or go.sum is not tidy; run `go mod tidy` in the repository")
    } else if has("no required module provides package") {
        Some("an import is in no module of go.mod or the go.work `use` list; run `go mod tidy` in the repository")
    } else if has("dial tcp") || has("no such host") || has("connection refused") {
        Some("a module download failed; check the network or GOPROXY, or run `go mod download` once online")
    } else {
        None
    }
}

fn with_hint(tail: &str, stderr: &str) -> String {
    hint(stderr).map_or_else(|| tail.to_string(), |h| format!("({h}) {tail}"))
}

/// One package of `go list -json`.
#[derive(Debug, Clone)]
struct Pkg {
    import_path: String,
    /// Repo-relative folder.
    dir: String,
    /// Repo-relative files the build (tests included) compiles.
    files: Vec<String>,
    /// The test files among them.
    tests: BTreeSet<String>,
    /// Why go cannot load it (`Error`, else the first `DepsErrors`), `pos: err`.
    error: Option<String>,
}

/// The `go list -json` fields read. The embed fields make go list resolve
/// `//go:embed` patterns, so their errors show in `Error` and `DepsErrors`.
const LIST_FIELDS: &str = "-json=ImportPath,Dir,GoFiles,CgoFiles,TestGoFiles,XTestGoFiles,EmbedFiles,TestEmbedFiles,XTestEmbedFiles,Error,DepsErrors";

fn parse_list(stdout: &[u8], copy: &Path) -> Vec<Pkg> {
    let err_of = |e: &Value| -> Option<String> {
        let text = e.get("Err").and_then(Value::as_str)?;
        let pos = e.get("Pos").and_then(Value::as_str).unwrap_or("");
        Some(if pos.is_empty() { text.to_string() } else { format!("{pos}: {text}") })
    };
    let mut out = Vec::new();
    for v in serde_json::Deserializer::from_slice(stdout).into_iter::<Value>().map_while(Result::ok) {
        let error = v.get("Error").and_then(err_of).or_else(|| v.get("DepsErrors").and_then(Value::as_array).and_then(|a| a.iter().find_map(err_of)));
        let Some(dir) = v.get("Dir").and_then(Value::as_str).and_then(|d| super::rel_to(copy, d)) else { continue };
        let mut files = Vec::new();
        let mut tests = BTreeSet::new();
        for key in ["GoFiles", "CgoFiles", "TestGoFiles", "XTestGoFiles"] {
            for name in v.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                let f = change::join(&dir, name);
                if key.contains("Test") {
                    tests.insert(f.clone());
                }
                files.push(f);
            }
        }
        out.push(Pkg { import_path: v.get("ImportPath").and_then(Value::as_str).unwrap_or("").to_string(), dir, files, tests, error });
    }
    out
}

/// One `go vet` pass: the GOOS and the package patterns.
#[derive(Debug, Clone)]
struct Pass {
    goos: String,
    patterns: Vec<String>,
}

#[derive(Default)]
struct VetRun {
    hits: Vec<Hit>,
    /// "cannot inline … into a file using goX" diagnostics.
    blocked: Vec<Refusal>,
    notes: Vec<String>,
    failed: BTreeMap<String, Failed>,
    /// The error lines of a failed run.
    errors: Vec<String>,
}

/// One vet run. `Err` when it failed without JSON and without naming the
/// packages that failed (`# pkg` headers), or timed out; packages it names
/// are in `failed`, so a later GOOS can retry them.
fn vet(go: &Go, site: &Site, pass: &Pass, mod_mod: bool) -> Result<VetRun, String> {
    let mut run = VetRun::default();
    // Chunks keep the command line under the Windows limit.
    for chunk in pass.patterns.chunks(200) {
        let out = go.run(go.cmd(site, &pass.goos)?.args(vet_args(&go.fix, &go.off, mod_mod, chunk)))?;
        let parsed = parse_vet(&out.stdout);
        let stderr = out.stderr_text();
        let failed = failed_packages(&stderr, &parsed.packages);
        if !out.ok() && parsed.objects == 0 {
            if stderr.contains("no packages to vet") {
                continue;
            }
            if failed.is_empty() {
                return Err(with_hint(&out.tail(4), &stderr));
            }
        }
        if !out.ok() {
            let error = with_hint(&out.tail(3), &stderr);
            run.notes.push(format!("go vet (GOOS={}) reported errors: {error}", pass.goos));
            run.errors.push(format!("GOOS={}: {error}", pass.goos));
        }
        run.failed.extend(failed);
        if let Some(e) = parsed.broken {
            run.notes.push(format!("go vet JSON (GOOS={}) stopped early: {e}", pass.goos));
        }
        run.notes.extend(parsed.errors.into_iter().map(|e| format!("go fix analyzer error: {e}")));
        for d in parsed.diags {
            // An inline diagnostic without a fix is the inliner giving up, not a finding.
            if d.analyzer == "inline" && !d.has_fix {
                if let Some((callee, declared)) = parse_refusal(&d.message)
                    && let Some(file) = super::rel_to(&site.copy, &d.path)
                {
                    run.blocked.push(Refusal { file, line: d.line, callee, declared });
                }
                continue;
            }
            let Some(file) = super::rel_to(&site.copy, &d.path) else { continue };
            run.hits.push(Hit { file, line: d.line, column: d.column, analyzer: d.analyzer, message: d.message, goos: pass.goos.clone() });
        }
    }
    Ok(run)
}

fn list(go: &Go, site: &Site, goos: &str, patterns: &[String]) -> Result<Vec<Pkg>, String> {
    let mut pkgs = Vec::new();
    for chunk in patterns.chunks(200) {
        let cmd = go.cmd(site, goos)?.args(["list", "-e", LIST_FIELDS]).args(chunk);
        let out = go.run(cmd)?;
        if !out.ok() {
            return Err(with_hint(&out.tail(4), &out.stderr_text()));
        }
        pkgs.extend(parse_list(&out.stdout, &site.copy));
    }
    Ok(pkgs)
}

/// What the base copy showed.
struct Base {
    hits: Vec<Hit>,
    passes: Vec<Pass>,
    blocked: Vec<Refusal>,
    uncovered: BTreeSet<String>,
    timed_out: bool,
}

/// `go list` over `./...` for the host GOOS, then over the folders of the
/// changed files no earlier pass compiled for linux, windows and darwin;
/// each pass vets only the packages that hold changed files. `Err` only for
/// a host pass that could not run, or when no GOOS analysed any changed file
/// and some failed (a package that type-checks only on Linux is retried there).
fn base_passes(go: &Go, m: &Module, site: &Site, notes: &mut Vec<String>) -> Result<Base, String> {
    let mut base = Base { hits: Vec::new(), passes: Vec::new(), blocked: Vec::new(), uncovered: m.wanted.clone(), timed_out: false };
    let mut errors: Vec<String> = Vec::new();
    let mut analysed_any = false;
    let mut load_errors: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    let order: Vec<&str> = std::iter::once(go.host_os.as_str()).chain(OTHER_GOOS.iter().copied().filter(|g| *g != go.host_os)).collect();
    for (i, goos) in order.into_iter().enumerate() {
        if base.uncovered.is_empty() {
            break;
        }
        let patterns: Vec<String> = if i == 0 {
            vec!["./...".into()]
        } else {
            base.uncovered.iter().map(|f| m.pattern(change::parent(f))).collect::<BTreeSet<_>>().into_iter().collect()
        };
        let pkgs = match list(go, site, goos, &patterns) {
            Ok(p) => p,
            Err(_) if go.budget.spent() => {
                base.timed_out = true;
                break;
            }
            Err(e) if i == 0 => return Err(format!("go list failed: {e}")),
            Err(e) => {
                notes.push(format!("{}: GOOS={goos} go list failed: {e}", m.label()));
                continue;
            }
        };
        let (broken, holding): (Vec<&Pkg>, Vec<&Pkg>) =
            pkgs.iter().filter(|p| p.files.iter().any(|f| base.uncovered.contains(f))).partition(|p| p.error.is_some());
        // A package go cannot load would fail the whole vet run: it is left
        // out of this pass and retried under the next GOOS (a Linux-only
        // import loads there); an error every GOOS repeats ends as a note.
        for p in &broken {
            let e = p.error.clone().unwrap_or_default();
            errors.push(format!("GOOS={goos}: go cannot load {}: {}", p.import_path, with_hint(&e, &e)));
            load_errors.entry(p.import_path.clone()).or_insert_with(|| (e, p.files.clone()));
        }
        if holding.is_empty() {
            continue;
        }
        let pass = Pass { goos: goos.to_string(), patterns: holding.iter().map(|p| m.pattern(&p.dir)).collect::<BTreeSet<_>>().into_iter().collect() };
        let run = match vet(go, site, &pass, false) {
            Ok(r) => r,
            Err(_) if go.budget.spent() => {
                base.timed_out = true;
                break;
            }
            Err(e) if i == 0 => return Err(format!("go vet failed: {e}")),
            Err(e) => {
                notes.push(format!("{}: GOOS={goos} go vet failed: {e}", m.label()));
                errors.push(format!("GOOS={goos}: {e}"));
                continue;
            }
        };
        notes.extend(run.notes.iter().map(|n| format!("{}: {n}", m.label())));
        errors.extend(run.errors.iter().cloned());
        let mut newly = 0usize;
        let mut analysed: BTreeSet<String> = BTreeSet::new();
        for p in &holding {
            let failed = run.failed.get(&p.import_path).copied();
            if failed != Some(Failed::Package) {
                analysed.insert(m.pattern(&p.dir));
            }
            for f in &p.files {
                if failed == Some(Failed::Package) || (failed == Some(Failed::Tests) && p.tests.contains(f)) {
                    continue;
                }
                newly += usize::from(base.uncovered.remove(f));
            }
        }
        analysed_any |= newly > 0;
        if i > 0 && newly > 0 {
            notes.push(format!("{}: GOOS={goos} pass analysed {newly} file(s) the host build leaves out.", m.label()));
        }
        base.blocked.extend(run.blocked);
        base.hits.extend(run.hits.into_iter().filter(|h| m.wanted.contains(&h.file)));
        // The raised copy reruns only the packages this pass analysed.
        if !analysed.is_empty() {
            base.passes.push(Pass { goos: pass.goos, patterns: analysed.into_iter().collect() });
        }
    }
    if !base.timed_out && !analysed_any && !errors.is_empty() {
        return Err(format!("no GOOS analysed a changed file: {}", errors.join(" || ")));
    }
    for (pkg, (e, files)) in &load_errors {
        if files.iter().any(|f| base.uncovered.contains(f)) {
            notes.push(format!("{}: {pkg} not analysed, go cannot load it: {}", m.label(), with_hint(e, e)));
            for f in files {
                base.uncovered.remove(f);
            }
        }
    }
    Ok(base)
}

/// Copies a module and runs its passes. `Some` names what was not analysed
/// when the time budget ran out.
fn run_module(ctx: &Ctx, report: &mut StackReport, go: &Go, m: &Module, index: usize, newest: &str, catalogue: &HashSet<(String, u32)>) -> Option<String> {
    let label = m.label().to_string();
    let Some(version) = m.version else {
        report.fail(format!("{label}: unreadable go line `{}` in {}", m.pin_value(), m.go_mod));
        return None;
    };
    if version > go.version {
        report.fail(format!(
            "{label}: {} needs go {}, newer than the installed {}; modern-tara never downloads a toolchain (GOTOOLCHAIN=local).",
            m.go_mod,
            m.pin_value(),
            go.version_text
        ));
        return None;
    }
    if !m.ignored.is_empty() {
        report.note(format!("{label}: the go tool never builds {} (testdata, vendor, _ or . names, //go:build ignore); not analysed.", short_list(&m.ignored)));
    }
    if m.wanted.is_empty() {
        return None;
    }
    let sources = sources(ctx.change, m);
    let site = match make_site(ctx, m, &format!("go-{index}"), &sources) {
        Ok(s) => s,
        Err(e) => {
            report.fail(e);
            return None;
        }
    };
    if let Some(ws) = &m.workspace {
        report.note(format!("{label}: in the workspace {} (copied, GOWORK set to the copy).", ws.file));
    }
    let mut notes = Vec::new();
    let base = base_passes(go, m, &site, &mut notes);
    for n in notes {
        report.note(n);
    }
    let base = match base {
        Ok(b) => b,
        Err(e) => {
            report.fail(format!("{label}: {e}"));
            return None;
        }
    };
    report.findings.extend(base_findings(&base.hits, catalogue));
    if base.timed_out {
        return Some(short_list(&base.uncovered.iter().cloned().collect::<Vec<_>>()));
    }
    if !base.uncovered.is_empty() {
        report.note(format!("{label}: not analysed under any GOOS: {}", short_list(&base.uncovered.iter().cloned().collect::<Vec<_>>())));
    }
    let raise = (version.major, version.minor) < (go.version.major, go.version.minor);
    if !raise || base.passes.is_empty() {
        if !base.blocked.is_empty() {
            report.note(format!("{label}: {} call(s) the inline analyzer cannot inline into a file with an older Go version.", base.blocked.len()));
        }
        return None;
    }
    raised_passes(ctx, report, go, m, version, &sources, &base, index, newest, catalogue).then(|| format!("the needs-newer check of {label}"))
}

/// A fresh copy of `sources` and where go runs in it.
fn make_site(ctx: &Ctx, m: &Module, name: &str, sources: &[String]) -> Result<Site, String> {
    let copy = ctx.temp.sub(name)?;
    ctx.change.materialize(&copy, sources)?;
    let cwd = if m.root.is_empty() { copy.clone() } else { change::safe_join(&copy, &m.root).unwrap_or_else(|| copy.clone()) };
    let gowork = m.workspace.as_ref().and_then(|w| change::safe_join(&copy, &w.file));
    Ok(Site { copy, cwd, gowork })
}

/// The raised copy (`go` line = the toolchain), and the minors in between
/// for the findings only it shows. `true` when the time budget ran out.
#[expect(clippy::too_many_arguments, reason = "one module's state, passed down once")]
fn raised_passes(
    ctx: &Ctx,
    report: &mut StackReport,
    go: &Go,
    m: &Module,
    version: GoVersion,
    sources: &[String],
    base: &Base,
    index: usize,
    newest: &str,
    catalogue: &HashSet<(String, u32)>,
) -> bool {
    let label = m.label();
    let full = go.version_text.strip_prefix("go").filter(|v| pins::go_version(v).is_some()).map_or_else(|| go.version.minor_text(), String::from);
    let site = match make_site(ctx, m, &format!("go-{index}-raised"), sources) {
        Ok(s) => s,
        Err(e) => {
            report.note(e);
            return false;
        }
    };
    let go_mod = site.cwd.join("go.mod");
    let Ok(text) = fs::read_to_string(&go_mod) else {
        report.note(format!("{label}: raised copy has no go.mod"));
        return false;
    };
    let set_go = |v: &str| fs::write(&go_mod, pins::raise_go_mod(&text, v)).map_err(|e| format!("{}: {e}", go_mod.display()));
    // A workspace's go line must be at least each module's.
    let work_raised = site.gowork.as_ref().is_none_or(|w| fs::read_to_string(w).and_then(|t| fs::write(w, pins::raise_go_mod(&t, &full))).is_ok());
    if let Err(e) = set_go(&full).and_then(|()| if work_raised { Ok(()) } else { Err("go.work could not be raised".into()) }) {
        report.note(e);
        return false;
    }
    // A raised go line can need more go.mod lines (module graph pruning); the
    // copy may change them, except in a vendored module or a workspace.
    let mod_mod = m.workspace.is_none() && !site.cwd.join("vendor").join("modules.txt").is_file();
    let mut raised = Vec::new();
    for pass in &base.passes {
        match vet(go, &site, pass, mod_mod) {
            Ok(run) => raised.extend(run.hits),
            Err(_) if go.budget.spent() => return true,
            Err(e) => {
                report.note(format!("{label}: the raised copy (go {full}) did not load, so no needs-newer findings: {e}"));
                return false;
            }
        }
    }
    let (mut extra, by_key) = raised_only(&base.hits, raised, &m.wanted, catalogue);
    let source = |f: &str| fs::read_to_string(site.copy.join(f)).ok();
    let toolchain = go.version.minor_text();
    let is_std = |p: &str| go.is_std(p);
    let std_now = std_inlines(&mut extra, &base.blocked, go.version, &source, &is_std);
    for h in &std_now {
        let mut f = h.finding(Kind::Modern, None);
        f.message = format!("{} (std replacement exists; go fix applies it once the go line reaches {toolchain})", f.message);
        report.findings.push(f);
    }
    report.note(format!(
        "{label}: raised copy (go {full}) analysed; {} finding(s) appear only there (needs-newer){}.",
        extra.len(),
        if std_now.is_empty() { String::new() } else { format!(", {} std inline call(s) reported as modern", std_now.len()) }
    ));
    if extra.is_empty() {
        return false;
    }

    // The first minor that shows each one: only its package folders, per GOOS.
    let mut first_seen: HashMap<Key3, u32> = HashMap::new();
    let mut steps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for h in extra.values() {
        steps.entry(h.goos.clone()).or_default().insert(m.pattern(change::parent(&h.file)));
    }
    let mut timed_out = false;
    if version.major == go.version.major {
        'steps: for minor in version.minor + 1..go.version.minor {
            if first_seen.len() == extra.len() {
                break;
            }
            if set_go(&format!("{}.{minor}", version.major)).is_err() {
                break;
            }
            for (goos, patterns) in &steps {
                let pass = Pass { goos: goos.clone(), patterns: patterns.iter().cloned().collect() };
                match vet(go, &site, &pass, mod_mod) {
                    Ok(run) => {
                        for h in run.hits {
                            if let Some(k) = by_key.get(&h.key5()).filter(|k| extra.contains_key(*k)) {
                                first_seen.entry(k.clone()).or_insert(minor);
                            }
                        }
                    }
                    Err(_) if go.budget.spent() => {
                        timed_out = true;
                        break 'steps;
                    }
                    Err(_) => {}
                }
            }
        }
    }
    let unplaced = extra.len() - first_seen.len();
    for (key, h) in &extra {
        let to = first_seen.get(key).map_or_else(|| toolchain.clone(), |n| format!("{}.{n}", version.major));
        let upgrade =
            Upgrade { pin: PIN.into(), from: m.pin_value(), to: to.clone(), note: format!("go fix reports it from go {to}; newest stable Go is {newest}") };
        report.findings.push(h.finding(Kind::NeedsNewer, Some(upgrade)));
    }
    if timed_out && unplaced > 0 {
        report.note(format!("{label}: time ran out while stepping minors; {unplaced} needs-newer finding(s) keep `to` {toolchain}, the toolchain's."));
    }
    timed_out
}

/// `a.go, b.go, c.go +4 more`.
fn short_list(files: &[String]) -> String {
    let shown: Vec<&str> = files.iter().take(8).map(String::as_str).collect();
    let more = files.len().saturating_sub(shown.len());
    if more > 0 { format!("{} +{more} more", shown.join(", ")) } else { shown.join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::Source;

    fn hit(file: &str, line: u32, column: u32, analyzer: &str, message: &str) -> Hit {
        Hit { file: file.into(), line, column, analyzer: analyzer.into(), message: message.into(), goos: "linux".into() }
    }

    #[test]
    fn posn_from_the_right() {
        assert_eq!(parse_posn(r"C:\Users\x\AppData\Local\Temp\mt\config.go:170:23"), Some((r"C:\Users\x\AppData\Local\Temp\mt\config.go".into(), 170, 23)));
        assert_eq!(parse_posn("C:/T/mt-ş/işçi/aşk.go:7:1"), Some(("C:/T/mt-ş/işçi/aşk.go".into(), 7, 1)));
        assert_eq!(parse_posn("/tmp/mt/a.go:3:9"), Some(("/tmp/mt/a.go".into(), 3, 9)));
        assert_eq!(parse_posn(r"C:\t\a.go:12"), Some((r"C:\t\a.go".into(), 12, 0)), "no column");
        assert_eq!(parse_posn("a.go:x:1"), None);
        assert_eq!(parse_posn("a.go"), None);
        assert_eq!(parse_posn(":1:2"), None);
        assert_eq!(parse_posn("-"), None);
    }

    #[test]
    fn vet_stream_with_clean_packages_errors_and_variants() {
        let stream = r#"{}
{
	"example.com/m": {
		"any": [
			{"posn": "C:\\T\\m\\main.go:10:17", "end": "C:\\T\\m\\main.go:10:28", "message": "interface{} can be replaced by any",
			 "category": "x", "related": [], "suggested_fixes": [{"message": "Replace", "edits": []}]}
		],
		"inline": [
			{"posn": "C:\\T\\m\\main.go:23:25", "end": "C:\\T\\m\\main.go:23:25", "message": "cannot inline call to ioutil.ReadFile (declared using go1.27.4) into a file using go1.21"}
		],
		"stringsseq": {"error": "analysis failed"}
	}
}
{"example.com/m_test [example.com/m.test]": {"any": [{"posn": "C:\\T\\m\\m_test.go:4:8", "message": "interface{} can be replaced by any", "suggested_fixes": [{}]}]}}
{"#;
        let v = parse_vet(stream.as_bytes());
        assert_eq!(v.objects, 3);
        assert!(v.broken.is_some(), "a cut object stops the stream");
        assert_eq!(v.errors, vec!["example.com/m stringsseq: analysis failed"]);
        assert_eq!(v.packages.into_iter().collect::<Vec<_>>(), vec!["example.com/m"], "test variants fold into the package");
        assert_eq!(v.diags.len(), 3);
        assert_eq!((v.diags[0].analyzer.as_str(), v.diags[0].line, v.diags[0].column, v.diags[0].has_fix), ("any", 10, 17, true));
        assert!(!v.diags[1].has_fix && parse_refusal(&v.diags[1].message).is_some());
        assert!(parse_refusal("cannot inline call to f: body refers to unexported g").is_none(), "only the go-version refusal counts");
        assert_eq!(v.diags[2].path, r"C:\T\m\m_test.go");
        let empty = parse_vet(b"");
        assert_eq!((empty.objects, empty.diags.len(), empty.broken.is_none()), (0, 0, true));
        assert_eq!(parse_vet(b"{}\n{}\n").objects, 2);
    }

    #[test]
    fn classification() {
        assert_eq!(confidence("any"), Confidence::High);
        assert_eq!(confidence("inline"), Confidence::High);
        assert_eq!(confidence("waitgroupgo"), Confidence::High);
        assert_eq!(confidence("omitzero"), Confidence::Low, "changes encoding/json output");
        assert_eq!(confidence("futuremodernizer"), Confidence::Low, "not catalogued yet");
        assert!(DISABLED.iter().all(|d| !ANALYZERS.contains(d)));
        let f = hit("a.go", 3, 1, "rangeint", "m").finding(Kind::Modern, None);
        assert_eq!((f.rule.as_str(), f.stack, f.source, f.confidence), ("go/rangeint", "go", "tool", Confidence::High));
        assert!(f.stops_gate());
        let low = hit("a.go", 4, 1, "omitzero", "m").finding(Kind::Modern, None);
        assert_eq!(low.confidence, Confidence::Low);
        assert!(!low.stops_gate());
    }

    #[test]
    fn failed_headers_spare_analysed_packages_and_scope_test_variants() {
        let stderr = "go: downloading x v1\n# example.com/m/util\nfix.exe: util\\bad.go:3:13: cannot use\n# example.com/m/b [example.com/m/b.test]\n# example.com/m/c_test [example.com/m/c.test]\n# example.com/m/d [example.com/m/d.test]\n";
        let analysed = BTreeSet::from(["example.com/m/d".to_string()]);
        let failed = failed_packages(stderr, &analysed);
        assert_eq!(
            failed.into_iter().collect::<Vec<_>>(),
            vec![("example.com/m/b".into(), Failed::Tests), ("example.com/m/c".into(), Failed::Tests), ("example.com/m/util".into(), Failed::Package)]
        );
        assert_eq!(installed_version("go1.27.4"), pins::go_version("1.27.4"));
        assert_eq!(installed_version("go1.27.4 X:boringcrypto"), pins::go_version("1.27.4"));
        assert_eq!(installed_version("devel go1.28-abcdef Mon Nov 2 2026"), pins::go_version("1.28"));
        assert_eq!(installed_version("unknown"), None);
    }

    #[test]
    fn raised_only_compares_column_and_message_and_skips_catalogued_inline() {
        let base = vec![hit("a.go", 10, 5, "any", "interface{} can be replaced by any")];
        let wanted = BTreeSet::from(["a.go".to_string()]);
        let catalogue = HashSet::from([("a.go".to_string(), 20)]);
        let raised = vec![
            hit("a.go", 10, 5, "any", "interface{} can be replaced by any"),
            hit("a.go", 10, 30, "any", "interface{} can be replaced by any"),
            hit("a.go", 20, 10, "inline", "Call of ioutil.ReadAll should be inlined"),
            hit("a.go", 21, 10, "inline", "Call of lib.Old should be inlined"),
            hit("b.go", 1, 1, "rangeint", "m"),
        ];
        let (extra, by_key) = raised_only(&base, raised, &wanted, &catalogue);
        assert_eq!(extra.keys().cloned().collect::<Vec<_>>(), vec![("a.go".into(), 10, "any".into()), ("a.go".into(), 21, "inline".into())]);
        assert_eq!(by_key.len(), 2);
        let found = base_findings(&[hit("a.go", 20, 10, "inline", "x"), hit("a.go", 3, 1, "any", "m"), hit("a.go", 3, 9, "any", "m")], &catalogue);
        assert_eq!(found.len(), 1, "one per line and rule, none for a catalogued inline");
    }

    #[test]
    fn call_shape_env_flags_and_budget() {
        assert!(GO_ENV.contains(&("GOTOOLCHAIN", "local")) && GO_ENV.contains(&("GO111MODULE", "on")) && GO_ENV.contains(&("GOFLAGS", "-buildvcs=false")));
        let names: Vec<String> = ["json", "any", "buildtag", "hostport"].iter().map(|s| (*s).to_string()).collect();
        let off = disable_flags(&names);
        assert_eq!(off, vec!["-buildtag=false", "-hostport=false"]);
        assert_eq!(disable_flags(&["json".to_string()]), Vec::<String>::new(), "only flags the tool has");
        let fix = Path::new("/go/pkg/tool/x/fix");
        let args = vet_args(fix, &off, true, &["./board".to_string()]);
        assert_eq!(args, vec!["vet", "-mod=mod", &format!("-vettool={}", fix.display()), "-json", "-buildtag=false", "-hostport=false", "./board"]);
        assert!(!vet_args(fix, &off, false, &[]).contains(&"-mod=mod".to_string()));
        assert_eq!(fix_path("/t").file_name().unwrap().to_string_lossy(), format!("fix{}", std::env::consts::EXE_SUFFIX));
        let budget = Budget::new(Duration::ZERO);
        assert!(budget.left().is_none() && budget.spent());
        let budget = Budget::new(Duration::from_secs(60));
        assert!(budget.left().is_some() && !budget.spent());
        budget.spend();
        assert!(budget.left().is_none());
    }

    #[test]
    fn load_failure_hints() {
        assert!(hint("web\\web.go:7:12: pattern dist: no matching files found").unwrap().contains("//go:embed"));
        assert!(hint("go: updates to go.mod needed; to update it:\n\tgo mod tidy").unwrap().contains("go mod tidy"));
        assert!(hint("main.go:3:8: no required module provides package example.com/lib; to add it:").unwrap().contains("go.work"));
        assert!(hint("dial tcp: lookup proxy.golang.org: no such host").unwrap().contains("GOPROXY"));
        assert_eq!(hint("type error"), None);
        assert_eq!(with_hint("x", "y"), "x");
        assert!(hint("pattern dist: no matching files found").unwrap().ends_with("build them before the scan"));
    }

    #[test]
    fn list_reads_load_errors() {
        let copy = Path::new("/t/c");
        let out = br#"{"ImportPath": "e/web", "Dir": "/t/c/web", "GoFiles": ["web.go"], "Error": {"Pos": "web/web.go:5:12", "Err": "pattern dist: no matching files found"}}
{"ImportPath": "e/uses", "Dir": "/t/c/uses", "GoFiles": ["u.go"], "DepsErrors": [{"Err": "pattern dist: no matching files found"}]}
{"ImportPath": "e/api", "Dir": "/t/c/api", "GoFiles": ["api.go"]}"#;
        let pkgs = parse_list(out, copy);
        assert_eq!(pkgs[0].error.as_deref(), Some("web/web.go:5:12: pattern dist: no matching files found"));
        assert_eq!(pkgs[1].error.as_deref(), Some("pattern dist: no matching files found"));
        assert_eq!(pkgs[2].error, None);
        assert!(LIST_FIELDS.contains("EmbedFiles") && LIST_FIELDS.contains("Error") && LIST_FIELDS.contains("DepsErrors"));
    }

    #[test]
    fn std_inline_refusals_become_modern_only_for_std_callees() {
        let msg = "cannot inline call to ioutil.ReadFile (declared using go1.27.4) into a file using go1.21";
        assert_eq!(parse_refusal(msg), Some(("ioutil.ReadFile".into(), "go1.27.4".into())));
        assert_eq!(parse_refusal("cannot inline call to f: body refers to g"), None);
        assert_eq!(inline_callee("Call of (*archsimd.Int8x16).Merge should be inlined"), Some("(*archsimd.Int8x16).Merge"));
        assert_eq!(qualifier("(*archsimd.Int8x16).Merge"), Some("archsimd"));
        assert_eq!(qualifier("ioutil.ReadFile"), Some("ioutil"));
        assert_eq!(qualifier("Old"), None);
        let src =
            "package p\n\nimport (\n\t\"io/ioutil\"\n\tr2 \"math/rand/v2\"\n\t\"example.com/x/lib\"\n\t_ \"embed\"\n)\n\nimport \"go/types\"\n\nfunc F() {}\n";
        assert_eq!(import_path_of(src, "ioutil").as_deref(), Some("io/ioutil"));
        assert_eq!(import_path_of(src, "r2").as_deref(), Some("math/rand/v2"));
        assert_eq!(import_path_of(src, "types").as_deref(), Some("go/types"));
        assert_eq!(import_path_of(src, "lib").as_deref(), Some("example.com/x/lib"));
        assert_eq!((import_path_of(src, "main"), import_path_of(src, "embed")), (None, None));
        assert_eq!(import_path_of("package p\nimport \"math/rand/v2\"\n", "rand").as_deref(), Some("math/rand/v2"), "a /vN element is not the name");
        assert!(dotless("io/ioutil") && !dotless("example.com/x/lib"));

        let toolchain = pins::go_version("1.27.4").unwrap();
        let refused = |callee: &str, line: u32, declared: &str| Refusal { file: "a.go".into(), line, callee: callee.into(), declared: declared.into() };
        let refusals = vec![refused("ioutil.ReadFile", 3, "go1.27.4"), refused("lib.Old", 4, "go1.27.4"), refused("types.NewSignature", 5, "go1.24")];
        let mut extra: BTreeMap<Key3, Hit> = BTreeMap::new();
        for (line, callee) in [(3, "ioutil.ReadFile"), (4, "lib.Old"), (5, "types.NewSignature")] {
            let h = hit("a.go", line, 2, "inline", &format!("Call of {callee} should be inlined"));
            extra.insert(h.key3(), h);
        }
        let source = |_: &str| Some(src.to_string());
        let std_list: BTreeSet<&str> = ["io/ioutil", "go/types", "math/rand/v2"].into();
        let is_std = |p: &str| std_list.contains(p);
        let mut kept = extra.clone();
        let moved = std_inlines(&mut extra, &refusals, toolchain, &source, &is_std);
        assert_eq!(moved.iter().map(|h| h.line).collect::<Vec<_>>(), vec![3], "std and declared with the toolchain's Go");
        assert_eq!(extra.len(), 2, "a non-std callee and an older declaration stay needs-newer");
        // A dot-less main module (`module myapp`) is not std when `go list std` says so.
        let app = "package p\n\nimport \"myapp/ioutil\"\n";
        let mine = |_: &str| Some(app.to_string());
        assert!(std_inlines(&mut kept, &refusals, toolchain, &mine, &is_std).is_empty());
        assert_eq!(std_inlines(&mut kept, &refusals, toolchain, &mine, &|p: &str| dotless(p)).len(), 1, "the dot rule is only the fallback");
        assert!(INLINE_RULES.contains(&"go-ioutil-io-os") && !INLINE_RULES.contains(&"go-ioutil-readdir"), "a low rule never drops an inline finding");
    }

    #[test]
    fn go_tool_skips() {
        assert!(skipped_by_go("testdata/a.go") && skipped_by_go("x/testdata/y/a.go") && skipped_by_go("_old/a.go") && skipped_by_go(".x/a.go"));
        assert!(skipped_by_go("_a.go") && skipped_by_go("vendor/x/a.go"));
        assert!(!skipped_by_go("board/board.go") && !skipped_by_go("testdata.go") && !skipped_by_go("x/vendor_a.go"));
    }

    #[test]
    fn list_files_are_repo_relative() {
        let copy = Path::new(r"C:\T\mt-1-0\go-0");
        let out =
            br#"{"ImportPath": "example.com/m/board", "Dir": "C:\\T\\mt-1-0\\go-0\\svc\\board", "GoFiles": ["board.go"], "XTestGoFiles": ["board_test.go"]}
{"ImportPath": "fmt", "Dir": "C:\\Program Files\\Go\\src\\fmt", "GoFiles": ["print.go"]}"#;
        let pkgs = parse_list(out, copy);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].dir, "svc/board");
        assert_eq!(pkgs[0].files, vec!["svc/board/board.go", "svc/board/board_test.go"]);
        assert_eq!(pkgs[0].tests.iter().collect::<Vec<_>>(), vec!["svc/board/board_test.go"]);
    }

    fn write_tree(root: &Path, files: &[(&str, &str)]) {
        let _ = fs::remove_dir_all(root);
        for (p, body) in files {
            let f = root.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, body).unwrap();
        }
    }

    #[test]
    fn copy_set_keeps_go_folders_and_leaves_nested_modules_out() {
        let root = std::env::temp_dir().join(format!("mt-go-copy-{}", std::process::id()));
        write_tree(
            &root,
            &[
                ("svc/go.mod", "module example.com/svc\n\ngo 1.21\n\nreplace example.com/lib => ../lib\n"),
                ("svc/go.sum", ""),
                ("svc/main.go", "package main"),
                ("svc/web/index.html", "<p>"),
                ("svc/vendor/modules.txt", "# x"),
                ("svc/vendor/example.com/dep/dep.go", "package dep"),
                ("svc/tools/go.mod", "module example.com/tools\n"),
                ("svc/tools/t.go", "package tools"),
                ("svc/gen.go", "//go:build ignore\n\npackage main"),
                ("lib/go.mod", "module example.com/lib\n"),
                ("lib/lib.go", "package lib"),
                ("docs/readme.md", "x"),
                ("README.md", "x"),
            ],
        );
        let change = Change::new(root.clone(), Source::Dir, BTreeMap::new(), serde_json::json!({}));
        let set = copy_set(&change, "svc");
        assert_eq!(
            set,
            vec![
                "lib/go.mod",
                "lib/lib.go",
                "svc/gen.go",
                "svc/go.mod",
                "svc/go.sum",
                "svc/main.go",
                "svc/vendor/example.com/dep/dep.go",
                "svc/vendor/modules.txt",
                "svc/web/index.html"
            ]
        );
        let changed: Vec<String> =
            ["svc/main.go", "svc/gen.go", "svc/vendor/example.com/dep/dep.go", "svc/tools/t.go", "docs/x.go"].iter().map(|s| (*s).to_string()).collect();
        let (modules, loose) = locate(&change, &changed);
        assert_eq!(loose, vec!["docs/x.go"]);
        assert_eq!(modules.len(), 2);
        assert_eq!((modules[0].root.as_str(), modules[0].go_text.as_deref()), ("svc", Some("1.21")));
        assert_eq!(modules[0].wanted.iter().collect::<Vec<_>>(), vec!["svc/main.go"], "vendor and //go:build ignore are never analysed");
        assert_eq!(modules[0].ignored, vec!["svc/gen.go", "svc/vendor/example.com/dep/dep.go"]);
        assert_eq!(modules[1].version, Some(GoVersion::lang(1, 16)), "a missing go line means 1.16");
        assert!(modules[1].pin_label().contains("no go line"), "{}", modules[1].pin_label());
        assert_eq!(modules[0].pattern("svc"), ".");
        assert_eq!(modules[0].pattern("svc/web/x"), "./web/x");
        assert_eq!(modules[0].workspace, None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn workspace_modules_copy_the_go_work_and_every_use() {
        let root = std::env::temp_dir().join(format!("mt-go-work-{}", std::process::id()));
        write_tree(
            &root,
            &[
                ("go.work", "go 1.22\n\nuse (\n\t./app\n\t./lib\n)\n"),
                ("go.work.sum", ""),
                ("app/go.mod", "module example.com/app\n\ngo 1.22\n"),
                ("app/main.go", "package main"),
                ("lib/go.mod", "module example.com/lib\n\ngo 1.16\n"),
                ("lib/lib.go", "package lib"),
                ("other/go.mod", "module example.com/other\n"),
                ("other/o.go", "package other"),
            ],
        );
        let change = Change::new(root.clone(), Source::Dir, BTreeMap::new(), serde_json::json!({}));
        let (modules, _) = locate(&change, &["app/main.go".into(), "other/o.go".into()]);
        let ws = modules[0].workspace.clone().unwrap();
        assert_eq!((ws.file.as_str(), ws.uses.clone()), ("go.work", vec!["app".to_string(), "lib".to_string()]));
        assert_eq!(sources(&change, &modules[0]), vec!["app/go.mod", "app/main.go", "go.work", "go.work.sum", "lib/go.mod", "lib/lib.go"]);
        assert_eq!(modules[1].workspace, None, "a module the go.work does not use runs with GOWORK=off");
        let _ = fs::remove_dir_all(&root);
    }
}
