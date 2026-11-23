//! C#: `dotnet format style` and `dotnet format analyzers` with a fixed list
//! of modernizer diagnostics, on two copies of the touched projects (run in
//! parallel, restored in the copy). A tool-owned `.editorconfig` section turns
//! the rules on at suggestion level and prefers the modern forms.

use super::{Ctx, astgrep};
use crate::change::{self, Change};
use crate::pins::{self, NEWEST_CSHARP, NEWEST_DOTNET, Tfm};
use crate::proc::Cmd;
use crate::report::{Finding, Kind, StackReport};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Code-style rules (`dotnet format style`), with the construct and release.
pub const STYLE: [(&str, &str); 27] = [
    ("IDE0019", "pattern matching instead of `as` + null check (C# 7)"),
    ("IDE0020", "pattern matching instead of `is` + cast (C# 7)"),
    ("IDE0028", "collection initializer or expression (C# 12)"),
    ("IDE0038", "pattern matching instead of `is` + cast (C# 7)"),
    ("IDE0041", "`is null` check (C# 7)"),
    ("IDE0066", "switch expression (C# 8)"),
    ("IDE0078", "pattern combinators `and`/`or`/`not` (C# 9)"),
    ("IDE0083", "`is not` pattern (C# 9)"),
    ("IDE0161", "file-scoped namespace (C# 10)"),
    ("IDE0290", "primary constructor (C# 12)"),
    ("IDE0300", "collection expression for an array (C# 12)"),
    ("IDE0301", "collection expression for an empty collection (C# 12)"),
    ("IDE0302", "collection expression for stackalloc (C# 12)"),
    ("IDE0303", "collection expression for `Create()` (C# 12)"),
    ("IDE0304", "collection expression for a builder (C# 12)"),
    ("IDE0305", "collection expression for a fluent call (C# 12)"),
    ("IDE0306", "collection expression for `new` (C# 12)"),
    ("IDE0330", "`System.Threading.Lock` (C# 13 / .NET 9)"),
    ("IDE0360", "simplified property accessor with `field` (C# 14)"),
    ("IDE0090", "target-typed `new()` (C# 9)"),
    ("IDE0057", "range operator `[a..b]` (C# 8)"),
    ("IDE0056", "index operator `[^1]` (C# 8)"),
    ("IDE0074", "coalesce compound assignment `??=` (C# 8)"),
    ("IDE0031", "null propagation `?.` (C# 6)"),
    ("IDE0029", "null coalescing `??` (C# 2)"),
    ("IDE0030", "null coalescing `??` on a nullable value (C# 2)"),
    ("IDE0270", "`?? throw` (C# 7)"),
];

/// Findings that are `low`: the code fix is not behaviour-neutral. CA1866
/// swaps a culture-aware string comparison for an ordinal char one
/// (`"\u{ad}abc".StartsWith("a")` is true, `StartsWith('a')` false).
const LOW_CONFIDENCE: [&str; 1] = ["CA1866"];

/// Analyzer rules (`dotnet format analyzers`).
pub const ANALYZERS: [(&str, &str); 19] = [
    ("CA1510", "`ArgumentNullException.ThrowIfNull` (.NET 6)"),
    ("CA1511", "`ArgumentException.ThrowIfNullOrEmpty` (.NET 7)"),
    ("CA1512", "`ArgumentOutOfRangeException.ThrowIf*` (.NET 8)"),
    ("CA1513", "`ObjectDisposedException.ThrowIf` (.NET 7)"),
    ("CA1839", "`Environment.ProcessPath` (.NET 6)"),
    ("CA1854", "`TryGetValue` instead of `ContainsKey` + indexer"),
    ("CA1860", "`Count`/`Length` instead of `Any()`"),
    ("CA1863", "`CompositeFormat` (.NET 8)"),
    ("CA1870", "cached `SearchValues` (.NET 8)"),
    ("CA1877", "`Path.Combine`/`Path.Join` overloads (.NET 9)"),
    ("CA2263", "generic overload instead of `Type` argument (.NET 5+)"),
    ("SYSLIB1045", "`[GeneratedRegex]` (.NET 7)"),
    ("SYSLIB1054", "`[LibraryImport]` (.NET 7)"),
    ("CA1829", "`Length`/`Count` property instead of `Count()`"),
    ("CA1865", "char overload for a single-character string argument (.NET Core 2.1+)"),
    ("CA1866", "char overload for a single-character string argument (.NET Core 2.1+)"),
    ("CA1847", "char literal for a single-character lookup (.NET Core 2.1+)"),
    ("CA1858", "`StartsWith` instead of `IndexOf` compared with 0"),
    ("CA1872", "`Convert.ToHexString` (.NET 5)"),
];

/// Style preferences that make the rules above fire; appended last to the
/// copy's root `.editorconfig`, so a closer `.editorconfig` still wins.
const EDITORCONFIG: &str = "
# --- modern-tara (temp copy only) ---
[*.cs]
csharp_style_namespace_declarations = file_scoped:suggestion
csharp_style_prefer_primary_constructors = true:suggestion
dotnet_style_prefer_collection_expression = when_types_loosely_match:suggestion
csharp_style_implicit_object_creation_when_type_is_apparent = true:suggestion
csharp_style_prefer_switch_expression = true:suggestion
csharp_style_prefer_pattern_matching = true:suggestion
csharp_style_prefer_not_pattern = true:suggestion
csharp_style_pattern_matching_over_as_with_null_check = true:suggestion
csharp_style_pattern_matching_over_is_with_cast_check = true:suggestion
csharp_style_prefer_range_operator = true:suggestion
csharp_style_prefer_index_operator = true:suggestion
csharp_prefer_system_threading_lock = true:suggestion
csharp_style_prefer_simple_property_accessors = true:suggestion
dotnet_style_prefer_is_null_check_over_reference_equality_method = true:suggestion
dotnet_style_collection_initializer = true:suggestion
dotnet_style_coalesce_expression = true:suggestion
dotnet_style_null_propagation = true:suggestion
dotnet_style_prefer_compound_assignment = true:suggestion
";

const BUILD_FILES: [&str; 7] =
    ["directory.build.props", "directory.build.targets", "directory.packages.props", "global.json", "nuget.config", ".editorconfig", "directory.build.rsp"];

const COPY_EXTENSIONS: [&str; 17] = [
    "cs",
    "csproj",
    "props",
    "targets",
    "resx",
    "json",
    "xaml",
    "razor",
    "cshtml",
    "config",
    "editorconfig",
    "globalconfig",
    "xml",
    "txt",
    "settings",
    "manifest",
    "ico",
];

struct Project {
    path: String,
    props: BTreeMap<String, String>,
    tfm: Option<(String, Tfm)>,
}

#[expect(clippy::too_many_lines, reason = "one linear pipeline: pins, copy, run, parse")]
pub fn run(ctx: &Ctx, files: &[String]) -> StackReport {
    let mut report = StackReport::new("csharp", files.to_vec());
    report.newest("dotnet", NEWEST_DOTNET);
    report.newest("csharp", NEWEST_CSHARP);
    let Some(dotnet) = ctx.tools.dotnet.clone() else {
        report.skip("dotnet not found: install the .NET SDK.");
        return report;
    };
    let sdks = Cmd::new(&dotnet)
        .arg("--list-sdks")
        .cwd(&std::env::temp_dir())
        .env("DOTNET_NOLOGO", "1")
        .run()
        .ok()
        .filter(crate::proc::Output::ok)
        .map_or_default(|o| o.stdout_text());
    let installed: Vec<String> = sdks.lines().filter_map(|l| l.split_whitespace().next()).map(String::from).collect();
    report.tool = format!("dotnet format (SDK {})", installed.last().map_or("?", String::as_str));
    report.newest("installed_sdk", installed.last().cloned().unwrap_or_default());

    let change = ctx.change;
    let (projects, loose) = owning_projects(change, files);
    if !loose.is_empty() {
        report.note(format!("no .csproj above {} file(s); only catalogue rules ran there.", loose.len()));
    }
    for f in &loose {
        report.unscanned(f.clone(), "no .csproj above it: dotnet format did not run (catalogue rules only)");
    }
    let mut proj_info: BTreeMap<String, Project> = BTreeMap::new();
    for p in projects.keys() {
        let props = effective_props(change, p);
        let tfm = pins::lowest_tfm(&props);
        proj_info.insert(p.clone(), Project { path: p.clone(), props, tfm });
    }
    // The SDK, not the project's TargetFramework, decides whether the C# 15 checks exist.
    if !sdk_at_least(&installed, NEWEST_DOTNET.parse().unwrap_or(u32::MAX)) {
        report.note(format!("C# {NEWEST_CSHARP} checks need the .NET {NEWEST_DOTNET} SDK (catalogue only until then)."));
    }
    let mut pin_json = serde_json::Map::new();
    for p in proj_info.values() {
        pin_json.insert(p.path.clone(), json!(p.props));
    }
    let global = super::nearest(change, "", "global.json").and_then(|g| change.read_text(&g)).map_or_default(|t| pins::global_json(&t));
    report.pin("projects", Value::Object(pin_json));
    if !global.is_empty() {
        report.pin("global.json", json!(global));
    }

    let tool_files: Vec<String> = projects.values().flatten().cloned().collect();
    let mut copy_style = None;
    if !projects.is_empty() {
        let closure = reference_closure(change, projects.keys().cloned().collect());
        let sources = copy_set(change, &closure, &tool_files);
        let drop_global =
            global.get("sdk.version").is_some_and(|v| !installed.iter().any(|i| i == v)) && global.get("sdk.rollForward").is_none_or(|r| r == "disable");
        if drop_global {
            report.note(format!(
                "global.json pins SDK {} which is not installed; the copy runs on the installed SDK.",
                global.get("sdk.version").map_or("", String::as_str)
            ));
        }
        let mut copies = Vec::new();
        for name in ["csharp-style", "csharp-analyzers"] {
            match prepare_copy(ctx, name, &sources, projects.keys(), drop_global) {
                Ok(dir) => copies.push(dir),
                Err(e) => {
                    report.fail(e);
                    return report;
                }
            }
        }
        let style_ids: Vec<&str> = STYLE.iter().map(|(id, _)| *id).collect();
        let analyzer_ids: Vec<&str> = ANALYZERS.iter().map(|(id, _)| *id).collect();
        let results: Vec<Result<Pass, String>> = std::thread::scope(|s| {
            let a = s.spawn(|| format_pass(ctx, &dotnet, &copies[0], "style", &style_ids, &tool_files));
            let b = s.spawn(|| format_pass(ctx, &dotnet, &copies[1], "analyzers", &analyzer_ids, &tool_files));
            vec![a.join().unwrap_or_else(|_| Err("style pass panicked".into())), b.join().unwrap_or_else(|_| Err("analyzer pass panicked".into()))]
        });
        for r in results {
            match r {
                Ok(pass) => {
                    for n in pass.notes {
                        report.note(n);
                    }
                    report.findings.extend(pass.findings);
                }
                Err(e) => {
                    for f in &tool_files {
                        report.unscanned(f.clone(), "dotnet format did not finish");
                    }
                    report.fail(e);
                }
            }
        }
        report.ran(style_ids.iter().chain(&analyzer_ids).copied());
        copy_style = copies.into_iter().next();
    }

    let catalogue_dir = match copy_style {
        Some(d) => d,
        None => match ctx.temp.sub("csharp-catalogue") {
            Ok(d) => d,
            Err(e) => {
                report.note(e);
                return report;
            }
        },
    };
    let _ = change.materialize(&catalogue_dir, &loose);
    match astgrep::scan(ctx, &catalogue_dir, files) {
        Ok(Some(matches)) => {
            report.ran(astgrep::rule_ids(&ctx.home, "TargetFramework"));
            for m in matches.into_iter().filter(|m| m.gate == "TargetFramework") {
                let project = projects.iter().find(|(_, fs)| fs.contains(&m.file)).map(|(p, _)| p.clone());
                let tfm = project.as_ref().and_then(|p| proj_info.get(p)).and_then(|p| p.tfm.clone());
                let need = pins::parse_tfm(&m.min);
                let below = matches!((&tfm, need), (Some((_, t)), Some(Tfm::Core(a, b)))  if !pins::tfm_at_least(*t, a, b));
                let from = tfm.as_ref().map(|(name, _)| name.clone());
                let note = format!("newest stable is .NET {NEWEST_DOTNET} / C# {NEWEST_CSHARP}");
                report.findings.push(astgrep::to_finding("csharp", m, from.as_deref(), below, &note));
            }
        }
        Ok(None) => report.note("ast-grep not found: catalogue rules skipped."),
        Err(e) => report.fail(e),
    }
    drop_doubled_ide0090(&mut report.findings);
    report
}

/// Changed files grouped by their nearest `.csproj`; files with none apart.
fn owning_projects(change: &Change, files: &[String]) -> (BTreeMap<String, Vec<String>>, Vec<String>) {
    let mut by_dir: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in change.listing().unwrap_or_default().iter().filter(|p| change::extension(p) == "csproj") {
        by_dir.entry(change::parent(p).to_string()).or_default().push(p.clone());
    }
    let mut projects: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut loose = Vec::new();
    for f in files {
        match change::ancestors(change::parent(f)).iter().find_map(|d| by_dir.get(d).and_then(|v| v.first())) {
            Some(p) => projects.entry(p.clone()).or_default().push(f.clone()),
            None => loose.push(f.clone()),
        }
    }
    (projects, loose)
}

/// Nearest Directory.Build.props (and the ones it imports above), then the project.
fn effective_props(change: &Change, project: &str) -> BTreeMap<String, String> {
    let mut layers = Vec::new();
    let mut dir = change::parent(project).to_string();
    while let Some(props) = super::nearest(change, &dir, "Directory.Build.props") {
        let text = change.read_text(&props).unwrap_or_default();
        layers.push(pins::msbuild_props(&text));
        let props_dir = change::parent(&props).to_string();
        if props_dir.is_empty() || !pins::imports_parent_props(&text) {
            break;
        }
        dir = change::parent(&props_dir).to_string();
    }
    layers.reverse();
    layers.push(pins::msbuild_props(&change.read_text(project).unwrap_or_default()));
    pins::merge_props(&layers)
}

/// The projects and every project they reference, transitively.
fn reference_closure(change: &Change, start: Vec<String>) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue = start;
    while let Some(p) = queue.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        let text = change.read_text(&p).unwrap_or_default();
        for include in pins::project_references(&text) {
            let joined = change::join(change::parent(&p), &include.replace('\\', "/"));
            if let Some(n) = change::normalize(&joined)
                && change.exists(&n)
            {
                queue.push(n);
            }
        }
    }
    seen
}

fn copy_set(change: &Change, projects: &BTreeSet<String>, changed: &[String]) -> Vec<String> {
    let mut out: BTreeSet<String> = changed.iter().cloned().collect();
    let listing = change.listing().unwrap_or_default();
    for p in projects {
        let dir = change::parent(p);
        out.extend(change.under(dir, |f| COPY_EXTENSIONS.contains(&change::extension(f).as_str())));
        for anc in change::ancestors(dir) {
            let prefix = if anc.is_empty() { String::new() } else { format!("{anc}/") };
            for f in listing.iter().filter(|f| f.starts_with(&prefix) && !f[prefix.len()..].contains('/')) {
                let name = change::file_name(f).to_ascii_lowercase();
                if BUILD_FILES.contains(&name.as_str()) || name.ends_with(".globalconfig") {
                    out.insert(f.clone());
                }
            }
        }
    }
    out.into_iter().collect()
}

fn prepare_copy<'a>(ctx: &Ctx, name: &str, sources: &[String], projects: impl Iterator<Item = &'a String>, drop_global: bool) -> Result<PathBuf, String> {
    let dir = ctx.temp.sub(name)?;
    ctx.change.materialize(&dir, sources)?;
    if drop_global {
        let _ = fs::remove_file(dir.join("global.json"));
    }
    let editorconfig = dir.join(".editorconfig");
    let mut text = fs::read_to_string(&editorconfig).unwrap_or_else(|_| "root = true\n".into());
    text.push_str(EDITORCONFIG);
    for (id, _) in STYLE.iter().chain(ANALYZERS.iter()) {
        let _ = writeln!(text, "dotnet_diagnostic.{id}.severity = suggestion");
    }
    fs::write(&editorconfig, text).map_err(|e| format!("{}: {e}", editorconfig.display()))?;
    let mut slnx = String::from("<Solution>\n");
    for p in projects {
        let _ = writeln!(slnx, "  <Project Path=\"{}\" />", xml_escape(p));
    }
    slnx.push_str("</Solution>\n");
    fs::write(dir.join("modern-tara.slnx"), slnx).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

struct Pass {
    findings: Vec<Finding>,
    notes: Vec<String>,
}

fn dotnet_cmd(dotnet: &Path, cwd: &Path) -> Cmd {
    Cmd::new(dotnet)
        .cwd(cwd)
        // English messages (the format tool follows the OS UI language).
        .env("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", "1")
        .env("DOTNET_CLI_UI_LANGUAGE", "en-US")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
        .env("DOTNET_CLI_USE_MSBUILD_SERVER", "0")
        .env("MSBUILDDISABLENODEREUSE", "1")
        .env("UseSharedCompilation", "false")
}

fn format_pass(ctx: &Ctx, dotnet: &Path, copy: &Path, sub: &str, ids: &[&str], files: &[String]) -> Result<Pass, String> {
    let mut notes = Vec::new();
    let restore =
        dotnet_cmd(dotnet, copy).args(["restore", "modern-tara.slnx", "-nodeReuse:false", "--verbosity", "quiet"]).timeout(ctx.timeout_or(300)).run()?;
    if !restore.ok() {
        notes.push(format!("restore failed in the copy, analysis ran without packages: {}", restore.tail(2)));
    }
    let report_dir = copy.join(format!(".modern-tara-report-{sub}"));
    let mut cmd = dotnet_cmd(dotnet, copy)
        .args(["format", sub, "modern-tara.slnx", "--verify-no-changes", "--no-restore", "--severity", "info", "--diagnostics"])
        .args(ids)
        .arg("--report")
        .arg(&report_dir)
        .timeout(ctx.timeout_or(600));
    // A list too long for one command line: the whole solution is formatted
    // instead (one workspace load; findings are cut to the changed lines later).
    let include_len: usize = files.iter().map(|f| crate::proc::arg_len(f.as_ref())).sum();
    let with_include = cmd.line_len() + include_len + 12 < crate::proc::MAX_LINE;
    if with_include {
        cmd = cmd.arg("--include").args(files);
    }
    let out = cmd.run()?;
    if out.timed_out {
        return Err(format!("dotnet format {sub} timed out after {}s (raise --timeout)", out.ms / 1000));
    }
    // 0 = clean, 2 = --verify-no-changes found something.
    if !matches!(out.code, Some(0 | 2)) {
        return Err(format!("dotnet format {sub} failed (exit {:?}): {}", out.code, out.tail(3)));
    }
    let report_file = report_dir.join("format-report.json");
    let text = fs::read_to_string(&report_file).map_err(|e| format!("dotnet format {sub}: no report ({e}): {}", out.tail(2)))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("dotnet format {sub} report: {e}"))?;
    let table: BTreeMap<&str, &str> = STYLE.iter().chain(ANALYZERS.iter()).copied().collect();
    let mut findings = Vec::new();
    for doc in v.as_array().into_iter().flatten() {
        let Some(file) = doc.get("FilePath").and_then(Value::as_str).and_then(|p| super::rel_to(copy, p)) else { continue };
        for c in doc.get("FileChanges").and_then(Value::as_array).into_iter().flatten() {
            let id = c.get("DiagnosticId").and_then(Value::as_str).unwrap_or("");
            let Some(construct) = table.get(id) else { continue };
            let num = |k: &str| c.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
            let raw = c.get("FormatDescription").and_then(Value::as_str).unwrap_or("");
            let text = raw.split_once(&format!("{id}: ")).map_or(raw, |(_, t)| t).trim();
            findings.push(Finding {
                stack: "csharp",
                file: file.clone(),
                line: num("LineNumber"),
                column: num("CharNumber"),
                rule: id.to_string(),
                message: format!("{text} — {construct}"),
                kind: Kind::Modern,
                source: "tool",
                confidence: if LOW_CONFIDENCE.contains(&id) { crate::report::Confidence::Low } else { crate::report::Confidence::High },
                upgrade: None,
            });
        }
    }
    if out.stdout_text().contains("Warnings were encountered while loading the workspace") {
        notes.push(format!("dotnet format {sub}: MSBuild reported warnings while loading the projects."));
    }
    Ok(Pass { findings, notes })
}

/// Whether `dotnet --list-sdks` names an SDK of this major version or newer.
pub fn sdk_at_least(installed: &[String], major: u32) -> bool {
    installed.iter().any(|v| v.split('.').next().and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| m >= major))
}

/// Drops each IDE0090 (target-typed `new()`) finding on a file and line
/// another finding already reports at least as strongly (a low-confidence
/// one never takes a gate stop away).
fn drop_doubled_ide0090(findings: &mut Vec<Finding>) {
    let others: BTreeSet<(String, u32, bool)> = findings.iter().filter(|f| f.rule != "IDE0090").map(|f| (f.file.clone(), f.line, f.stops_gate())).collect();
    let covered = |f: &Finding, stops: bool| others.contains(&(f.file.clone(), f.line, stops));
    findings.retain(|f| f.rule != "IDE0090" || !(covered(f, true) || !f.stops_gate() && covered(f, false)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Confidence;

    fn finding(rule: &str, line: u32, source: &'static str) -> Finding {
        Finding {
            stack: "csharp",
            file: "A.cs".into(),
            line,
            column: 5,
            rule: rule.into(),
            message: String::new(),
            kind: Kind::Modern,
            confidence: Confidence::High,
            source,
            upgrade: None,
        }
    }

    #[test]
    fn only_ca1866_is_low() {
        assert_eq!(LOW_CONFIDENCE, ["CA1866"]);
        assert!(ANALYZERS.iter().any(|(id, _)| *id == "CA1866") && !LOW_CONFIDENCE.contains(&"CA1865") && !LOW_CONFIDENCE.contains(&"CA1847"));
    }

    #[test]
    fn installed_sdk_decides_the_csharp_15_note() {
        let list = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(sdk_at_least(&list(&["10.0.404", "11.0.100"]), 11));
        assert!(sdk_at_least(&list(&["12.0.100-preview.1.26101.1"]), 11));
        assert!(!sdk_at_least(&list(&["8.0.400", "10.0.404"]), 11));
        assert!(!sdk_at_least(&[], 11));
    }

    #[test]
    fn ide0090_is_dropped_where_another_finding_reports_the_line() {
        let mut v = vec![
            finding("IDE0090", 3, "tool"),
            finding("IDE0028", 3, "tool"),
            finding("IDE0090", 4, "tool"),
            finding("csharp-new-random", 4, "ast-grep"),
            finding("IDE0090", 7, "tool"),
            finding("IDE0090", 7, "tool"),
            // A low-confidence finding must not take the gate stop away.
            finding("IDE0090", 9, "tool"),
            Finding { confidence: Confidence::Low, ..finding("csharp-new-random", 9, "ast-grep") },
        ];
        drop_doubled_ide0090(&mut v);
        let left: Vec<(&str, u32)> = v.iter().map(|f| (f.rule.as_str(), f.line)).collect();
        assert_eq!(left, vec![("IDE0028", 3), ("csharp-new-random", 4), ("IDE0090", 7), ("IDE0090", 7), ("IDE0090", 9), ("csharp-new-random", 9)]);
    }
}
