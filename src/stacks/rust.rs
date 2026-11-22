//! Rust: clippy on a copy of the workspace, reporting only modern-idiom lints.
//! A second run on a copy with `rust-version` and `edition` raised finds what
//! the pins hold back (`needs-newer`). The target dir is a persistent cache
//! under the OS cache folder (`modern-tara/cache`), so only local crates rebuild.

use super::{Ctx, astgrep};
use crate::change::{self, Change};
use crate::pins::{self, NEWEST_EDITION, NEWEST_RUST};
use crate::proc::Cmd;
use crate::report::{Finding, Kind, StackReport, Upgrade};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Passed with `-W` besides `clippy::pedantic`.
pub const WARN: [&str; 15] = [
    "manual_let_else",
    "uninlined_format_args",
    "unnecessary_map_or",
    "manual_split_once",
    "non_std_lazy_statics",
    "needless_range_loop",
    "manual_strip",
    "manual_div_ceil",
    "manual_abs_diff",
    "manual_is_multiple_of",
    "manual_midpoint",
    "manual_inspect",
    "manual_repeat_n",
    "duration_suboptimal_units",
    "allow_attributes",
];

/// Further lints (default or pedantic) that point at a newer std API or
/// language form. Every `manual_*` lint counts too.
pub const MODERN_EXTRA: [&str; 38] = [
    "map_unwrap_or",
    "option_map_or_none",
    "result_map_or_into_option",
    "legacy_numeric_constants",
    "io_other_error",
    "from_iter_instead_of_collect",
    "iter_kv_map",
    "match_like_matches_macro",
    "mem_replace_with_default",
    "option_as_ref_deref",
    "redundant_static_lifetimes",
    "implicit_saturating_sub",
    "implicit_saturating_add",
    "cast_abs_to_unsigned",
    "checked_conversions",
    "ref_as_ptr",
    "ptr_as_ptr",
    "ptr_cast_constness",
    "borrow_as_ptr",
    "zero_ptr",
    "cloned_instead_of_copied",
    "flat_map_option",
    "map_flatten",
    "filter_map_identity",
    "unnecessary_get_then_check",
    "get_first",
    "unwrap_or_default",
    "bool_to_int_with_if",
    "unnested_or_patterns",
    "question_mark",
    "redundant_pattern_matching",
    "chars_next_cmp",
    "chars_last_cmp",
    "byte_char_slices",
    "lines_filter_map_ok",
    "assigning_clones",
    "while_let_on_iterator",
    "obfuscated_if_else",
];

/// Style lints that are only modern when the raised run alone reports them:
/// `collapsible_if` on `if let` nesting fires only with edition 2024 and
/// Rust 1.88+ (let chains). Recorded in both runs, reported only as needs-newer.
pub const RAISED_ONLY: [&str; 1] = ["collapsible_if"];

fn raised_only(name: &str) -> bool {
    RAISED_ONLY.contains(&name.trim_start_matches("clippy::"))
}

/// The Rust version `clippy::incompatible_msrv` names ("... stable since `1.80.0`").
pub fn msrv_needed(message: &str) -> Option<String> {
    let rest = &message[message.find("stable since `")? + "stable since `".len()..];
    let v = &rest[..rest.find('`')?];
    pins::rust_version(v).map(|(a, b)| format!("{a}.{b}"))
}

pub fn is_modern_lint(name: &str, message: &str) -> bool {
    let lint = name.trim_start_matches("clippy::");
    lint.starts_with("manual_")
        || WARN.contains(&lint)
        || MODERN_EXTRA.contains(&lint)
        || ["stabilized", "stable since", "since Rust", "MSRV"].iter().any(|k| message.contains(k))
}

struct Workspace {
    /// Repo-relative root ("" = repo root).
    root: String,
    /// Changed files in it.
    files: Vec<String>,
    edition: Option<String>,
    rust_version: Option<String>,
    crates: BTreeMap<String, Value>,
}

pub fn run(ctx: &Ctx, files: &[String]) -> StackReport {
    let mut report = StackReport::new("rust", files.to_vec());
    report.newest("rust", NEWEST_RUST);
    report.newest("edition", NEWEST_EDITION);
    let (Some(cargo), Some(rustc)) = (ctx.tools.cargo.clone(), ctx.tools.rustc.clone()) else {
        report.skip("cargo/rustc not found: install Rust with rustup (stable toolchain, clippy component).");
        return report;
    };
    let verbose = Cmd::new(&rustc).arg("-vV").cwd(&std::env::temp_dir()).run().ok().filter(crate::proc::Output::ok).map_or_default(|o| o.stdout_text());
    let host = verbose.lines().find_map(|l| l.strip_prefix("host: ")).unwrap_or("").trim().to_string();
    let toolchain = verbose.lines().find_map(|l| l.strip_prefix("release: ")).unwrap_or("").trim().to_string();
    let clippy = super::version_line(&cargo, &["clippy", "--version"]);
    let Some(clippy) = clippy else {
        report.skip("cargo clippy not available: `rustup component add clippy`.");
        return report;
    };
    report.tool = format!("{clippy} (rustc {toolchain})");
    let newest = pins::rust_version(&toolchain).map_or_else(
        || NEWEST_RUST.to_string(),
        |(a, b)| {
            let n = pins::rust_version(NEWEST_RUST).unwrap_or((a, b));
            let v = n.max((a, b));
            format!("{}.{}", v.0, v.1)
        },
    );
    report.newest("rust", newest.clone());

    let (workspaces, loose) = locate(ctx.change, files);
    if !loose.is_empty() {
        report.note(format!("no Cargo.toml above {} file(s); only catalogue rules ran there.", loose.len()));
    }
    let mut pin_json = serde_json::Map::new();
    for ws in &workspaces {
        pin_json.insert(
            if ws.root.is_empty() { ".".into() } else { ws.root.clone() },
            json!({"edition": ws.edition, "rust-version": ws.rust_version, "crates": ws.crates}),
        );
    }
    report.pin("workspaces", Value::Object(pin_json));

    let strip_cc = host.ends_with("-msvc");
    for (i, ws) in workspaces.iter().enumerate() {
        run_workspace(ctx, &mut report, &cargo, ws, i, strip_cc, &newest);
    }

    // Catalogue rules over every changed .rs file, from one plain copy.
    if let Ok(copy) = ctx.temp.sub("rust-catalogue") {
        let _ = ctx.change.materialize(&copy, files);
        match astgrep::scan(ctx, &copy, files) {
            Ok(Some(matches)) => {
                for m in matches.into_iter().filter(|m| m.gate == "rust-version") {
                    let rv = workspaces.iter().find(|w| w.files.contains(&m.file)).and_then(|w| w.rust_version.clone());
                    let below = matches!((rv.as_deref().and_then(pins::rust_version), pins::rust_version(&m.min)), (Some(have), Some(need)) if have < need);
                    let note = format!("newest stable Rust is {newest}");
                    report.findings.push(astgrep::to_finding("rust", m, rv.as_deref(), below, &note));
                }
            }
            Ok(None) => report.note("ast-grep not found: catalogue rules skipped."),
            Err(e) => report.note(e),
        }
    }
    report
}

fn run_workspace(ctx: &Ctx, report: &mut StackReport, cargo: &Path, ws: &Workspace, index: usize, strip_cc: bool, newest: &str) {
    let label = if ws.root.is_empty() { "." } else { ws.root.as_str() };
    let sources = copy_set(ctx.change, &ws.root);
    let target_dir = super::cache_dir(&ctx.change.root, &format!("rust|{}", ws.root));
    let _ = fs::create_dir_all(&target_dir);
    let _ = fs::write(target_dir.join("source.txt"), format!("{}\n{}\n", ctx.change.root.display(), label));
    let started = SystemTime::now();

    let base_copy = match ctx.temp.sub(&format!("rust-{index}")) {
        Ok(d) => d,
        Err(e) => return report.fail(e),
    };
    if let Err(e) = ctx.change.materialize(&base_copy, &sources) {
        return report.fail(e);
    }
    let names = package_names(&base_copy, &sources);
    let cwd = change::safe_join(&base_copy, &ws.root).unwrap_or_else(|| base_copy.clone());
    let base = clippy_run(ctx, cargo, &cwd, &target_dir, strip_cc);
    let base = match base {
        Ok(b) => b,
        Err(e) => return report.fail(format!("{label}: {e}")),
    };
    if let Some(err) = &base.error {
        report.fail(format!("{label}: clippy did not finish: {err}"));
    }
    let mut seen: HashSet<(String, u32, String)> = HashSet::new();
    for h in &base.hits {
        let file = change::join(&ws.root, &h.file);
        seen.insert((file.clone(), h.line, h.rule.clone()));
        report.findings.extend(base_finding(h, file, ws));
    }

    let edition_old = ws.edition.as_deref().unwrap_or("2015") < NEWEST_EDITION;
    let rv_old = ws.rust_version.as_deref().and_then(pins::rust_version).is_some_and(|v| Some(v) < pins::rust_version(newest));
    let msrv_file = sources.iter().any(|s| {
        let n = change::file_name(s);
        (n == "clippy.toml" || n == ".clippy.toml")
            && fs::read_to_string(base_copy.join(s)).is_ok_and(|t| t.lines().any(|l| l.trim_start().starts_with("msrv")))
    });
    if base.error.is_none() && (edition_old || rv_old || msrv_file) {
        let raised_copy = match ctx.temp.sub(&format!("rust-{index}-raised")) {
            Ok(d) => d,
            Err(e) => return report.note(e),
        };
        if let Err(e) = ctx.change.materialize(&raised_copy, &sources) {
            return report.note(e);
        }
        raise(&raised_copy, &sources, edition_old, rv_old || msrv_file, newest);
        let cwd = change::safe_join(&raised_copy, &ws.root).unwrap_or_else(|| raised_copy.clone());
        let what = match (edition_old, rv_old || msrv_file) {
            (true, true) => format!("edition {NEWEST_EDITION}, rust-version {newest}"),
            (true, false) => format!("edition {NEWEST_EDITION}"),
            _ => format!("rust-version {newest}"),
        };
        match clippy_run(ctx, cargo, &cwd, &target_dir, strip_cc) {
            Ok(raised) if raised.error.is_none() => {
                let mut added = 0usize;
                for h in &raised.hits {
                    let file = change::join(&ws.root, &h.file);
                    if seen.contains(&(file.clone(), h.line, h.rule.clone())) {
                        continue;
                    }
                    added += 1;
                    let upgrade = if raised_only(&h.rule) {
                        Upgrade {
                            pin: "edition".into(),
                            from: ws.edition.clone().unwrap_or_else(|| "2015".into()),
                            to: NEWEST_EDITION.into(),
                            note: format!(
                                "let chains need edition 2024 and Rust 1.88+{}",
                                if rv_old { format!("; also raise rust-version to 1.88+ (newest {newest})") } else { String::new() }
                            ),
                        }
                    } else if rv_old || msrv_file {
                        Upgrade {
                            pin: "rust-version".into(),
                            from: ws.rust_version.clone().unwrap_or_else(|| "clippy.toml msrv".into()),
                            to: newest.to_string(),
                            note: if edition_old { format!("the raised run also used edition {NEWEST_EDITION}") } else { String::new() },
                        }
                    } else {
                        Upgrade {
                            pin: "edition".into(),
                            from: ws.edition.clone().unwrap_or_else(|| "2015".into()),
                            to: NEWEST_EDITION.into(),
                            note: String::new(),
                        }
                    };
                    report.findings.push(h.finding(file, Kind::NeedsNewer, Some(upgrade)));
                }
                report.note(format!("{label}: raised copy ({what}) compiled; {added} clippy finding(s) appear only there (needs-newer)."));
            }
            Ok(raised) => {
                report.note(format!("{label}: the raised copy ({what}) did not compile, so no needs-newer findings: {}", raised.error.unwrap_or_default()));
            }
            Err(e) => report.note(format!("{label}: raised run failed: {e}")),
        }
    }
    prune(&target_dir, &names, started);
}

/// A base-run hit as a finding: `incompatible_msrv` is needs-newer (the
/// version it names), raised-only lints are not reported here.
fn base_finding(h: &Hit, file: String, ws: &Workspace) -> Option<Finding> {
    if h.rule == "clippy::incompatible_msrv"
        && let Some(to) = msrv_needed(&h.message)
    {
        let upgrade = Upgrade {
            pin: "rust-version".into(),
            from: ws.rust_version.clone().unwrap_or_else(|| "clippy.toml msrv".into()),
            to: to.clone(),
            note: format!("the code already uses an API stable since Rust {to}"),
        };
        return Some(h.finding(file, Kind::NeedsNewer, Some(upgrade)));
    }
    (!raised_only(&h.rule)).then(|| h.finding(file, Kind::Modern, None))
}

struct Hit {
    /// Relative to the workspace root.
    file: String,
    line: u32,
    column: u32,
    rule: String,
    message: String,
}

impl Hit {
    fn finding(&self, file: String, kind: Kind, upgrade: Option<Upgrade>) -> Finding {
        Finding {
            stack: "rust",
            file,
            line: self.line,
            column: self.column,
            rule: self.rule.clone(),
            message: self.message.clone(),
            kind,
            source: "tool",
            confidence: crate::report::Confidence::High,
            upgrade,
        }
    }
}

struct ClippyRun {
    hits: Vec<Hit>,
    /// First compile error, if the build did not succeed.
    error: Option<String>,
}

fn clippy_run(ctx: &Ctx, cargo: &Path, cwd: &Path, target_dir: &Path, strip_cc: bool) -> Result<ClippyRun, String> {
    let mut cmd = Cmd::new(cargo)
        .args(["clippy", "--message-format=json", "--all-targets", "--keep-going", "--color", "never", "--", "-W", "clippy::pedantic"])
        .cwd(cwd)
        .env("CARGO_TARGET_DIR", target_dir)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_TERM_COLOR", "never")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .timeout(ctx.timeout_or(900));
    for lint in WARN {
        cmd = cmd.arg("-W").arg(format!("clippy::{lint}"));
    }
    if strip_cc {
        // A user-wide CC (MinGW gcc) breaks MSVC links of crates with C code.
        cmd = cmd.env_remove("CC").env_remove("CXX");
    }
    let out = cmd.run()?;
    if out.timed_out {
        return Err(format!("cargo clippy timed out after {}s (raise --timeout)", out.ms / 1000));
    }
    let mut hits = Vec::new();
    let mut error: Option<String> = None;
    let mut finished_ok = None;
    for line in out.stdout_text().lines().filter(|l| l.starts_with('{')) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        match v.get("reason").and_then(Value::as_str) {
            Some("build-finished") => finished_ok = v.get("success").and_then(Value::as_bool),
            Some("compiler-message") => {
                let Some(msg) = v.get("message") else { continue };
                let level = msg.get("level").and_then(Value::as_str).unwrap_or("");
                let code = msg.pointer("/code/code").and_then(Value::as_str).unwrap_or("");
                let text = msg.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                let span =
                    msg.get("spans").and_then(Value::as_array).and_then(|s| s.iter().find(|x| x.get("is_primary").and_then(Value::as_bool) == Some(true)));
                let place = span.and_then(|s| {
                    let file = super::rel_to(cwd, s.get("file_name")?.as_str()?)?;
                    let n = |k: &str| s.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
                    Some((file, n("line_start")?, n("column_start")?))
                });
                if level == "error" && !code.starts_with("clippy::") {
                    if error.is_none() {
                        let at = place.as_ref().map_or_default(|(f, l, _)| format!(" at {f}:{l}"));
                        error = Some(format!("{text}{at}"));
                    }
                    continue;
                }
                if !code.starts_with("clippy::") || !(is_modern_lint(code, &text) || raised_only(code)) {
                    continue;
                }
                if let Some((file, line, column)) = place {
                    hits.push(Hit { file, line, column, rule: code.to_string(), message: text });
                }
            }
            _ => {}
        }
    }
    if finished_ok == Some(false) && error.is_none() {
        error = Some(out.tail(2));
    }
    if finished_ok.is_none() && !out.ok() {
        error = Some(out.tail(3));
    }
    Ok(ClippyRun { hits, error })
}

/// Workspaces holding the changed files, and files with no manifest above.
fn locate(change: &Change, files: &[String]) -> (Vec<Workspace>, Vec<String>) {
    let mut by_root: BTreeMap<String, Workspace> = BTreeMap::new();
    let mut loose = Vec::new();
    for f in files {
        let Some(manifest) = change::ancestors(change::parent(f)).into_iter().map(|d| change::join(&d, "Cargo.toml")).find(|m| change.exists(m)) else {
            loose.push(f.clone());
            continue;
        };
        let crate_dir = change::parent(&manifest).to_string();
        let text = change.read_text(&manifest).unwrap_or_default();
        let own = pins::cargo_pins(&text, None).unwrap_or_default();
        let mut root = crate_dir.clone();
        let mut root_text: Option<String> = None;
        if !own.is_workspace {
            for dir in change::ancestors(&crate_dir).into_iter().skip(1) {
                let candidate = change::join(&dir, "Cargo.toml");
                if !change.exists(&candidate) {
                    continue;
                }
                let t = change.read_text(&candidate).unwrap_or_default();
                if let Ok(p) = pins::cargo_pins(&t, None)
                    && p.is_workspace
                {
                    let rel = crate_dir.trim_prefix(format!("{dir}/").as_str());
                    let rel = if dir.is_empty() { crate_dir.as_str() } else { rel };
                    if pins::is_member(&p, rel) {
                        root = dir;
                        root_text = Some(t);
                    }
                    break;
                }
            }
        }
        let pins = pins::cargo_pins(&text, root_text.as_deref()).unwrap_or_default();
        let ws = by_root.entry(root.clone()).or_insert_with(|| Workspace {
            root: root.clone(),
            files: Vec::new(),
            edition: None,
            rust_version: None,
            crates: BTreeMap::new(),
        });
        ws.files.push(f.clone());
        let crate_name = pins.name.clone().unwrap_or_else(|| crate_dir.clone());
        ws.crates.insert(crate_name, json!({"manifest": manifest, "edition": pins.edition, "rust-version": pins.rust_version}));
        // The oldest pins of the touched crates gate the workspace.
        let edition = pins.edition.clone().unwrap_or_else(|| "2015".into());
        if ws.edition.as_ref().is_none_or(|e| edition < *e) {
            ws.edition = Some(edition);
        }
        if let Some(rv) = pins.rust_version
            && ws.rust_version.as_deref().and_then(pins::rust_version).is_none_or(|have| pins::rust_version(&rv).is_some_and(|v| v < have))
        {
            ws.rust_version = Some(rv);
        }
    }
    (by_root.into_values().collect(), loose)
}

/// Files to copy for a workspace: everything under its root, plus path
/// dependencies outside it. Toolchain pins stay out (no rustup installs).
fn copy_set(change: &Change, root: &str) -> Vec<String> {
    let mut dirs: BTreeSet<String> = BTreeSet::from([root.to_string()]);
    let mut queue = vec![root.to_string()];
    while let Some(dir) = queue.pop() {
        for manifest in change.under(&dir, |p| change::file_name(p) == "Cargo.toml") {
            let Some(text) = change.read_text(&manifest) else { continue };
            for dep in path_dependencies(&text) {
                let joined = change::join(change::parent(&manifest), &dep);
                if let Some(norm) = change::normalize(&joined)
                    && !dirs.iter().any(|d| d.is_empty() || norm == *d || norm.starts_with(&format!("{d}/")))
                {
                    dirs.insert(norm.clone());
                    queue.push(norm);
                }
            }
        }
    }
    let mut out: Vec<String> =
        dirs.iter().flat_map(|d| change.under(d, |p| !matches!(change::file_name(p), "rust-toolchain" | "rust-toolchain.toml"))).collect();
    out.sort();
    out.dedup();
    out
}

fn path_dependencies(text: &str) -> Vec<String> {
    let Ok(table) = text.parse::<toml::Table>() else { return Vec::new() };
    let mut out = Vec::new();
    let mut visit = |deps: Option<&toml::Value>| {
        if let Some(t) = deps.and_then(toml::Value::as_table) {
            out.extend(t.values().filter_map(|d| d.get("path").and_then(toml::Value::as_str).map(String::from)));
        }
    };
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        visit(table.get(key));
        visit(table.get("workspace").and_then(|w| w.get(key)));
    }
    if let Some(targets) = table.get("target").and_then(toml::Value::as_table) {
        for t in targets.values() {
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                visit(t.get(key));
            }
        }
    }
    out
}

fn package_names(copy: &Path, sources: &[String]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for s in sources.iter().filter(|s| change::file_name(s) == "Cargo.toml") {
        if let Ok(text) = fs::read_to_string(copy.join(s))
            && let Ok(p) = pins::cargo_pins(&text, None)
            && let Some(n) = p.name
        {
            names.insert(n.replace('-', "_"));
            names.insert(n);
        }
    }
    names.into_iter().collect()
}

/// Rewrites manifests (and clippy msrv) in the raised copy.
fn raise(copy: &Path, sources: &[String], edition: bool, rust_version: bool, newest: &str) {
    for s in sources {
        let path = copy.join(s);
        match change::file_name(s) {
            "Cargo.toml" => {
                if let Ok(text) = fs::read_to_string(&path) {
                    let _ = fs::write(&path, raise_manifest(&text, edition, rust_version, newest));
                }
            }
            "clippy.toml" | ".clippy.toml" if rust_version => {
                if let Ok(text) = fs::read_to_string(&path) {
                    let kept: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with("msrv")).collect();
                    let _ = fs::write(&path, kept.join("\n"));
                }
            }
            _ => {}
        }
    }
}

/// `edition` → 2024 (added when missing) and `rust-version` → `newest` in
/// `[package]` and `[workspace.package]`; inherited keys are left alone.
pub fn raise_manifest(text: &str, edition: bool, rust_version: bool, newest: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let mut section = String::new();
    let mut package_has_edition = false;
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let close_package = |section: &str, has: &mut bool, out: &mut String| {
        if section == "package" && edition && !*has {
            let _ = write!(out, "edition = \"{NEWEST_EDITION}\"{eol}");
            *has = true;
        }
    };
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            close_package(&section, &mut package_has_edition, &mut out);
            section = trimmed.trim_start_matches('[').split(']').next().unwrap_or("").trim().to_string();
            out.push_str(line);
            continue;
        }
        if section == "package" || section == "workspace.package" {
            let key = trimmed.split(['=', ' ', '.', '\t']).next().unwrap_or("");
            let inherited = trimmed.contains("workspace");
            if key == "edition" {
                if section == "package" {
                    package_has_edition = true;
                }
                if edition && !inherited {
                    let _ = write!(out, "edition = \"{NEWEST_EDITION}\"{eol}");
                    continue;
                }
            }
            if key == "rust-version" && rust_version && !inherited {
                let _ = write!(out, "rust-version = \"{newest}\"{eol}");
                continue;
            }
        }
        out.push_str(line);
    }
    if section == "package" && edition && !package_has_edition {
        if !out.ends_with('\n') {
            out.push_str(eol);
        }
        let _ = write!(out, "edition = \"{NEWEST_EDITION}\"{eol}");
    }
    out
}

/// Removes this repo's local-crate artifacts older than `since` from the
/// cache. The copy path changes every run, so each run leaves a new hash
/// behind; registry dependencies are kept.
fn prune(target: &Path, names: &[String], since: SystemTime) {
    if names.is_empty() {
        return;
    }
    let mut profiles: Vec<PathBuf> = vec![target.join("debug")];
    if let Ok(entries) = fs::read_dir(target) {
        profiles.extend(entries.flatten().map(|e| e.path().join("debug")).filter(|p| p.is_dir()));
    }
    for profile in profiles {
        for sub in [".fingerprint", "build", "deps", "examples", "incremental"] {
            let Ok(entries) = fs::read_dir(profile.join(sub)) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !artifact_of(&name, names) {
                    continue;
                }
                let stale = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < since);
                if stale {
                    let p = entry.path();
                    let _ = if p.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
                }
            }
        }
    }
}

/// `lib<name>-<16 hex>.rmeta`, `<name>-<16 hex>` and the like.
fn artifact_of(file: &str, names: &[String]) -> bool {
    let stem = file.split('.').next().unwrap_or(file);
    let Some((name, hash)) = stem.rsplit_once('-') else { return false };
    if hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let name = name.strip_prefix("lib").filter(|n| names.iter().any(|x| x == n)).unwrap_or(name);
    names.iter().any(|x| x == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raise_edition_and_rust_version() {
        let m = "[package]\r\nname = \"a\"\r\nedition = \"2021\"\r\nrust-version = \"1.70\"\r\n\r\n[dependencies]\r\nx = { path = \"../x\" }\r\n";
        let r = raise_manifest(m, true, true, "1.100");
        assert!(r.contains("edition = \"2024\"\r\n"));
        assert!(r.contains("rust-version = \"1.100\"\r\n"));
        assert!(r.contains("x = { path"));
    }

    #[test]
    fn raise_adds_missing_edition_and_keeps_inherited() {
        let r = raise_manifest("[package]\nname = \"a\"\n[lib]\n", true, false, "1.100");
        assert!(r.starts_with("[package]\nname = \"a\"\nedition = \"2024\"\n[lib]"));
        let r = raise_manifest("[package]\nname = \"a\"\nedition.workspace = true\n", true, true, "1.100");
        assert!(r.contains("edition.workspace = true") && !r.contains("2024"));
        let r = raise_manifest("[workspace.package]\nedition = \"2021\"\nrust-version = \"1.80\"\n", true, true, "1.100");
        assert!(r.contains("edition = \"2024\"") && r.contains("rust-version = \"1.100\""));
    }

    #[test]
    fn artifacts_matched_by_name() {
        let names = vec!["demo-tool".to_string(), "demo_tool".to_string()];
        assert!(artifact_of("libdemo_tool-0123456789abcdef.rmeta", &names));
        assert!(artifact_of("demo_tool-0123456789abcdef.d", &names));
        assert!(artifact_of("demo-tool-0123456789abcdef", &names));
        assert!(!artifact_of("serde-0123456789abcdef", &names));
        assert!(!artifact_of("demo_tool-xyz.d", &names));
    }

    #[test]
    fn modern_lint_list() {
        assert!(is_modern_lint("clippy::manual_let_else", ""));
        assert!(is_modern_lint("clippy::manual_ok_err", ""));
        assert!(is_modern_lint("clippy::io_other_error", ""));
        assert!(!is_modern_lint("clippy::doc_markdown", "item in documentation is missing backticks"));
        assert!(!is_modern_lint("clippy::must_use_candidate", ""));
        assert!(!is_modern_lint("clippy::collapsible_if", "") && raised_only("clippy::collapsible_if"));
        let m = "current MSRV (Minimum Supported Rust Version) is `1.70.0` but this item is stable since `1.80.0`";
        assert!(is_modern_lint("clippy::incompatible_msrv", m));
        assert_eq!(msrv_needed(m).as_deref(), Some("1.80"));
        assert_eq!(msrv_needed("no version here"), None);
    }

    #[test]
    fn path_deps() {
        let d = path_dependencies("[dependencies]\na = { path = \"../a\" }\nb = \"1\"\n[target.'cfg(windows)'.dependencies]\nc = { path = \"c\" }\n");
        assert_eq!(d, vec!["../a", "c"]);
    }
}
