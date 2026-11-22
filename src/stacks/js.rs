//! JS/TS (`ESLint`) and CSS (stylelint), both through `js/modern-tara.mjs`
//! with tool-owned configs from `js/`. Never reads a config from the repo.

use super::{Ctx, astgrep};
use crate::change::{self, Change};
use crate::pins::{self, NEWEST_ECMASCRIPT, NEWEST_NODE};
use crate::proc::Cmd;
use crate::report::{Confidence, Finding, Kind, StackReport, Upgrade};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Rule, first Node release with what it suggests, whether TypeScript can
/// compile the construct down (syntax), and the construct.
pub const RULES: [(&str, (u32, u32), bool, &str); 36] = [
    ("unicorn/prefer-at", (16, 6), false, "Array/String .at(), ES2022"),
    ("unicorn/prefer-structured-clone", (17, 0), false, "structuredClone()"),
    ("unicorn/prefer-node-protocol", (16, 0), false, "`node:` specifiers"),
    ("unicorn/prefer-string-replace-all", (15, 0), false, "String#replaceAll(), ES2021"),
    ("unicorn/prefer-array-find", (4, 0), false, "Array#find(), ES2015"),
    ("unicorn/prefer-array-flat-map", (11, 0), false, "Array#flatMap(), ES2019"),
    ("unicorn/prefer-array-some", (0, 10), false, "Array#some()"),
    ("unicorn/prefer-includes", (6, 0), false, "includes(), ES2016"),
    ("unicorn/prefer-number-properties", (4, 0), false, "Number.isNaN() and friends, ES2015"),
    ("unicorn/prefer-global-this", (12, 0), false, "globalThis, ES2020"),
    ("unicorn/prefer-module", (14, 0), false, "ES modules"),
    ("unicorn/prefer-top-level-await", (14, 8), false, "top-level await, ES2022"),
    ("unicorn/prefer-abort-signal-timeout", (17, 3), false, "AbortSignal.timeout()"),
    ("unicorn/prefer-url-can-parse", (19, 9), false, "URL.canParse()"),
    ("unicorn/prefer-set-methods", (22, 0), false, "Set methods, ES2025"),
    ("unicorn/prefer-iterator-helpers", (22, 0), false, "iterator helpers, ES2025"),
    ("unicorn/prefer-promise-with-resolvers", (22, 0), false, "Promise.withResolvers(), ES2024"),
    ("unicorn/prefer-import-meta-properties", (20, 11), false, "import.meta.dirname/filename"),
    ("unicorn/prefer-string-starts-ends-with", (4, 0), false, "startsWith()/endsWith(), ES2015"),
    ("unicorn/prefer-spread", (5, 0), true, "spread syntax, ES2015"),
    ("unicorn/prefer-modern-dom-apis", (0, 10), false, "modern DOM APIs"),
    ("unicorn/prefer-modern-math-apis", (4, 0), false, "Math.hypot()/log10() and friends, ES2015"),
    ("unicorn/prefer-object-from-entries", (12, 0), false, "Object.fromEntries(), ES2019"),
    ("unicorn/prefer-optional-catch-binding", (10, 0), true, "optional catch binding, ES2019"),
    ("unicorn/no-for-each", (0, 10), false, "for-of instead of forEach"),
    ("unicorn/prefer-group-by", (21, 0), false, "Object.groupBy()/Map.groupBy(), ES2024"),
    ("unicorn/prefer-array-from-async", (22, 0), false, "Array.fromAsync(), ES2026"),
    ("unicorn/prefer-regexp-escape", (24, 0), false, "RegExp.escape(), ES2025"),
    ("@typescript-eslint/prefer-optional-chain", (14, 0), true, "optional chaining, ES2020"),
    ("@typescript-eslint/prefer-nullish-coalescing", (14, 0), true, "nullish coalescing, ES2020"),
    ("@typescript-eslint/prefer-includes", (6, 0), false, "includes(), ES2016"),
    ("@typescript-eslint/prefer-string-starts-ends-with", (4, 0), false, "startsWith()/endsWith(), ES2015"),
    ("@typescript-eslint/prefer-for-of", (0, 12), true, "for-of, ES2015"),
    ("prefer-object-has-own", (16, 9), false, "Object.hasOwn(), ES2022"),
    ("no-var", (6, 0), true, "let/const, ES2015"),
    ("prefer-template", (4, 0), true, "template literals, ES2015"),
];
/// Also on, ES2015 like `prefer-template`.
const OBJECT_SHORTHAND: (&str, (u32, u32), bool, &str) = ("object-shorthand", (4, 0), true, "shorthand properties, ES2015");

const JS_EXT: [&str; 4] = ["js", "mjs", "cjs", "jsx"];

/// es-x rules enabled (ES2022+ features only) and the first Node release
/// that ships each feature unflagged. A JS file gets the rule only when its
/// `engines.node` minimum is below that release; findings are `baseline`.
/// Features unicorn also suggests must agree with `RULES` (tested).
pub const ES_X: [(&str, (u32, u32)); 54] = [
    // ES2022
    ("es-x/no-array-prototype-at", (16, 6)),
    ("es-x/no-string-prototype-at", (16, 6)),
    ("es-x/no-class-instance-fields", (12, 0)),
    ("es-x/no-class-private-fields", (12, 0)),
    ("es-x/no-class-static-fields", (12, 0)),
    ("es-x/no-class-private-methods", (14, 6)),
    ("es-x/no-class-static-block", (16, 11)),
    ("es-x/no-error-cause", (16, 9)),
    ("es-x/no-object-hasown", (16, 9)),
    ("es-x/no-private-in", (16, 4)),
    ("es-x/no-regexp-d-flag", (16, 0)),
    ("es-x/no-arbitrary-module-namespace-names", (16, 0)),
    ("es-x/no-top-level-await", (14, 8)),
    // ES2023
    ("es-x/no-array-prototype-findlast-findlastindex", (18, 0)),
    ("es-x/no-array-prototype-toreversed", (20, 0)),
    ("es-x/no-array-prototype-tosorted", (20, 0)),
    ("es-x/no-array-prototype-tospliced", (20, 0)),
    ("es-x/no-array-prototype-with", (20, 0)),
    ("es-x/no-hashbang", (12, 0)),
    // ES2024
    ("es-x/no-object-groupby", (21, 0)),
    ("es-x/no-map-groupby", (21, 0)),
    ("es-x/no-promise-withresolvers", (22, 0)),
    ("es-x/no-regexp-v-flag", (20, 0)),
    ("es-x/no-arraybuffer-prototype-transfer", (21, 0)),
    ("es-x/no-resizable-and-growable-arraybuffers", (20, 0)),
    ("es-x/no-string-prototype-iswellformed", (20, 0)),
    ("es-x/no-string-prototype-towellformed", (20, 0)),
    ("es-x/no-atomics-waitasync", (16, 0)),
    // ES2025
    ("es-x/no-set-prototype-union", (22, 0)),
    ("es-x/no-set-prototype-intersection", (22, 0)),
    ("es-x/no-set-prototype-difference", (22, 0)),
    ("es-x/no-set-prototype-symmetricdifference", (22, 0)),
    ("es-x/no-set-prototype-issubsetof", (22, 0)),
    ("es-x/no-set-prototype-issupersetof", (22, 0)),
    ("es-x/no-set-prototype-isdisjointfrom", (22, 0)),
    ("es-x/no-iterator", (22, 0)),
    ("es-x/no-iterator-prototype-map", (22, 0)),
    ("es-x/no-iterator-prototype-filter", (22, 0)),
    ("es-x/no-iterator-prototype-take", (22, 0)),
    ("es-x/no-iterator-prototype-drop", (22, 0)),
    ("es-x/no-iterator-prototype-flatmap", (22, 0)),
    ("es-x/no-iterator-prototype-reduce", (22, 0)),
    ("es-x/no-iterator-prototype-toarray", (22, 0)),
    ("es-x/no-iterator-prototype-foreach", (22, 0)),
    ("es-x/no-iterator-prototype-some", (22, 0)),
    ("es-x/no-iterator-prototype-every", (22, 0)),
    ("es-x/no-iterator-prototype-find", (22, 0)),
    ("es-x/no-import-attributes", (18, 20)),
    ("es-x/no-promise-try", (23, 0)),
    ("es-x/no-regexp-modifiers", (23, 0)),
    ("es-x/no-regexp-duplicate-named-capturing-groups", (23, 0)),
    ("es-x/no-regexp-escape", (24, 0)),
    ("es-x/no-float16array", (24, 0)),
    // ES2026
    ("es-x/no-array-fromasync", (22, 0)),
];

/// The same feature in both tables: (es-x rule, `RULES` rule).
#[cfg(test)]
pub const SAME_FEATURE: [(&str, &str); 12] = [
    ("es-x/no-array-prototype-at", "unicorn/prefer-at"),
    ("es-x/no-object-hasown", "prefer-object-has-own"),
    ("es-x/no-top-level-await", "unicorn/prefer-top-level-await"),
    ("es-x/no-set-prototype-union", "unicorn/prefer-set-methods"),
    ("es-x/no-iterator-prototype-map", "unicorn/prefer-iterator-helpers"),
    ("es-x/no-promise-withresolvers", "unicorn/prefer-promise-with-resolvers"),
    ("es-x/no-string-prototype-at", "unicorn/prefer-at"),
    ("es-x/no-set-prototype-intersection", "unicorn/prefer-set-methods"),
    ("es-x/no-object-groupby", "unicorn/prefer-group-by"),
    ("es-x/no-map-groupby", "unicorn/prefer-group-by"),
    ("es-x/no-array-fromasync", "unicorn/prefer-array-from-async"),
    ("es-x/no-regexp-escape", "unicorn/prefer-regexp-escape"),
];

/// es-x rules a file with this `engines.node` minimum needs.
pub fn esx_rules_for(floor: (u32, u32, u32)) -> Vec<&'static str> {
    ES_X.iter().filter(|(_, min)| (floor.0, floor.1) < *min).map(|(id, _)| *id).collect()
}

const TS_EXT: [&str; 4] = ["ts", "mts", "cts", "tsx"];

/// Only `.js` and `.jsx` can be classic scripts; `.mjs`, `.cjs` and
/// TypeScript keep their own module kind.
fn is_classic_ext(ext: &str) -> bool {
    matches!(ext, "js" | "jsx")
}

struct PackageInfo {
    path: Option<String>,
    engines: Option<String>,
    module: bool,
    browserslist: Vec<String>,
}

fn package_for(change: &Change, file: &str) -> PackageInfo {
    let mut info = PackageInfo { path: None, engines: None, module: false, browserslist: Vec::new() };
    for dir in change::ancestors(change::parent(file)) {
        let rc = change::join(&dir, ".browserslistrc");
        if info.browserslist.is_empty() && change.exists(&rc) {
            info.browserslist = pins::browserslistrc(&change.read_text(&rc).unwrap_or_default());
        }
        let pj = change::join(&dir, "package.json");
        if info.path.is_none()
            && change.exists(&pj)
            && let Some(text) = change.read_text(&pj)
            && let Ok(v) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}'))
        {
            info.path = Some(pj);
            info.engines = v.pointer("/engines/node").and_then(Value::as_str).map(String::from);
            info.module = v.get("type").and_then(Value::as_str) == Some("module");
            if info.browserslist.is_empty()
                && let Some(b) = v.get("browserslist")
            {
                info.browserslist = pins::browserslist_from_json(b);
            }
        }
        if info.path.is_some() && !info.browserslist.is_empty() {
            break;
        }
    }
    info
}

fn driver_ready(ctx: &Ctx, report: &mut StackReport, package: &str) -> Option<std::path::PathBuf> {
    let Some(node) = ctx.tools.node.clone() else {
        report.skip("node not found: install Node.js.");
        return None;
    };
    let js = ctx.home.join("js");
    if !js.join("node_modules").join(package).join("package.json").is_file() {
        report.skip(format!("{package} is not installed: run `npm ci` in {}.", js.display()));
        return None;
    }
    if !js.join("modern-tara.mjs").is_file() {
        report.fail(format!("{} is missing", js.join("modern-tara.mjs").display()));
        return None;
    }
    Some(node)
}

fn run_driver(ctx: &Ctx, node: &Path, job: &Value, name: &str, default_timeout: u64) -> Result<Value, String> {
    let job_file = ctx.temp.path().join(format!("{name}-job.json"));
    fs::write(&job_file, serde_json::to_vec(job).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let script = ctx.home.join("js").join("modern-tara.mjs");
    let out = Cmd::new(node)
        .arg("--no-warnings")
        .arg(&script)
        .arg(&job_file)
        .cwd(&ctx.home.join("js"))
        .env("NODE_OPTIONS", "")
        .env("NO_COLOR", "1")
        .env("BROWSERSLIST_IGNORE_OLD_DATA", "1")
        .timeout(ctx.timeout_or(default_timeout))
        .run()?;
    if out.timed_out {
        return Err(format!("{name} timed out after {}s (raise --timeout)", out.ms / 1000));
    }
    if !out.ok() {
        return Err(format!("{name} driver failed: {}", out.tail(4)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("{name} driver output: {e}: {}", out.tail(2)))
}

#[expect(clippy::too_many_lines, reason = "one linear pipeline: pins, copy, run, parse")]
pub fn run_js(ctx: &Ctx, files: &[String]) -> StackReport {
    let mut report = StackReport::new("js", files.to_vec());
    report.newest("node", NEWEST_NODE);
    report.newest("ecmascript", NEWEST_ECMASCRIPT);
    let Some(node) = driver_ready(ctx, &mut report, "eslint") else { return report };
    let change = ctx.change;

    let mut copy: BTreeSet<String> = files.iter().cloned().collect();
    let listing = change.listing().unwrap_or_default();
    for p in listing {
        let n = change::file_name(p).to_ascii_lowercase();
        if n == "package.json" || (n.starts_with("tsconfig") && change::extension(&n) == "json") || n == "jsconfig.json" {
            copy.insert(p.clone());
        }
    }
    let mut entries = Vec::new();
    let mut pins_json = serde_json::Map::new();
    let mut untyped = 0usize;
    let mut kept_kind = 0usize;
    let mut ts_dirs: BTreeSet<String> = BTreeSet::new();
    let mut infos = Vec::new();
    for f in files {
        let ext = change::extension(f);
        let info = package_for(change, f);
        let is_ts = TS_EXT.contains(&ext.as_str());
        let tsconfig = if is_ts { super::nearest(change, change::parent(f), "tsconfig.json") } else { None };
        if is_ts && tsconfig.is_none() {
            untyped += 1;
        }
        if let Some(t) = &tsconfig {
            ts_dirs.insert(change::parent(t).to_string());
        }
        let classic = ctx.classic_scripts.iter().any(|g| g.matches(f));
        if classic && !is_classic_ext(&ext) {
            kept_kind += 1;
        }
        let source_type = match ext.as_str() {
            _ if classic && is_classic_ext(&ext) => "script",
            "mjs" | "mts" => "module",
            "cjs" | "cts" => "commonjs",
            _ if is_ts || info.module => "module",
            _ => "commonjs",
        };
        let esx: Vec<&str> = if is_ts { Vec::new() } else { info.engines.as_deref().and_then(pins::node_min).map_or_default(esx_rules_for) };
        entries.push(json!({"path": f, "sourceType": source_type, "typeAware": tsconfig.is_some(), "ts": is_ts, "esx": esx}));
        pins_json.insert(
            f.clone(),
            json!({
                "package.json": info.path,
                "engines.node": info.engines,
                "type": if info.module { "module" } else { "commonjs" },
                // The kind used for the parse (the requested one until the driver says otherwise).
                "sourceType": source_type,
                "tsconfig": tsconfig,
            }),
        );
        infos.push((f.clone(), info, is_ts));
    }
    for d in &ts_dirs {
        copy.extend(change.under(d, |p| {
            let e = change::extension(p);
            JS_EXT.contains(&e.as_str()) || TS_EXT.contains(&e.as_str()) || e == "json"
        }));
    }
    report.pin("files", Value::Object(pins_json.clone()));
    if kept_kind > 0 {
        report.note(format!("{kept_kind} file(s) in classic-scripts keep their own module kind (.mjs/.cjs/TS)"));
    }
    if untyped > 0 {
        report.note(format!("{untyped} TypeScript file(s) have no tsconfig.json: type-aware rules (prefer-optional-chain, prefer-nullish-coalescing, prefer-includes, prefer-string-starts-ends-with) skipped there."));
    }

    let dir = match ctx.temp.sub("js") {
        Ok(d) => d,
        Err(e) => {
            report.fail(e);
            return report;
        }
    };
    let copy: Vec<String> = copy.into_iter().collect();
    if let Err(e) = change.materialize(&dir, &copy) {
        report.fail(e);
        return report;
    }
    let job = json!({
        "mode": "eslint",
        "root": dir.to_string_lossy(),
        "repo": change.root.to_string_lossy(),
        "files": entries,
    });
    let out = match run_driver(ctx, &node, &job, "eslint", 300) {
        Ok(v) => v,
        Err(e) => {
            report.fail(e);
            return report;
        }
    };
    report.tool = out.get("tool").and_then(Value::as_str).unwrap_or("eslint").to_string();
    for n in out.get("notes").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        report.note(n);
    }
    for w in out.get("warnings").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        report.warn(w);
    }
    // The kind each file finally parsed as (a classic script that is really a module was re-linted as one).
    if let Some(kinds) = out.get("kinds").and_then(Value::as_object) {
        for (file, kind) in kinds {
            if let Some(pin) = pins_json.get_mut(file).and_then(Value::as_object_mut) {
                pin.insert("sourceType".into(), kind.clone());
            }
        }
        report.pin("files", Value::Object(pins_json));
    }
    let table: Vec<_> = RULES.iter().chain(std::iter::once(&OBJECT_SHORTHAND)).collect();
    for r in out.get("results").and_then(Value::as_array).into_iter().flatten() {
        let Some(file) = r.get("file").and_then(Value::as_str).and_then(change::normalize) else { continue };
        let rule = r.get("rule").and_then(Value::as_str).unwrap_or("").to_string();
        let Some((_, info, is_ts)) = infos.iter().find(|(f, ..)| *f == file) else { continue };
        let num = |k: &str| r.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
        if rule.starts_with("es-x/") {
            let engines = info.engines.clone().unwrap_or_default();
            let Some((_, (major, minor))) = ES_X.iter().find(|(id, _)| *id == rule) else { continue };
            let text = r.get("message").and_then(Value::as_str).unwrap_or("");
            report.findings.push(Finding {
                stack: "js",
                file,
                line: num("line"),
                column: num("column"),
                rule,
                message: format!("{text} (engines.node {engines}; Node ships it from {major}.{minor})"),
                kind: Kind::Baseline,
                source: "tool",
                confidence: crate::report::Confidence::High,
                upgrade: None,
            });
            continue;
        }
        let Some(&&(_, min, syntax, construct)) = table.iter().find(|(id, ..)| *id == rule) else { continue };
        let floor = info.engines.as_deref().and_then(pins::node_min);
        let below = floor.is_some_and(|(a, b, _)| (a, b) < min) && !(*is_ts && syntax);
        report.findings.push(Finding {
            stack: "js",
            file,
            line: num("line"),
            column: num("column"),
            rule,
            message: r.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
            kind: if below { Kind::NeedsNewer } else { Kind::Modern },
            source: "tool",
            confidence: crate::report::Confidence::High,
            upgrade: below.then(|| Upgrade {
                pin: "engines.node".into(),
                from: info.engines.clone().unwrap_or_default(),
                to: NEWEST_NODE.into(),
                note: format!("{construct} needs Node {}.{}+", min.0, min.1),
            }),
        });
    }

    collapse_prefer_module(change, &mut report.findings);

    match astgrep::scan(ctx, &dir, files) {
        Ok(Some(matches)) => {
            for m in matches.into_iter().filter(|m| m.gate == "engines.node") {
                let info = infos.iter().find(|(f, ..)| *f == m.file).map(|(_, i, _)| i);
                let engines = info.and_then(|i| i.engines.clone());
                let floor = engines.as_deref().and_then(pins::node_min);
                let need = pins::major_minor(&m.min);
                let below = matches!((floor, need), (Some((a, b, _)), Some(n)) if (a, b) < n);
                let why = format!("needs Node {}+", m.min);
                let mut f = astgrep::to_finding("js", m, engines.as_deref(), below, &why);
                if let Some(u) = &mut f.upgrade {
                    u.to = NEWEST_NODE.into();
                }
                report.findings.push(f);
            }
        }
        Ok(None) => report.note("ast-grep not found: catalogue rules skipped."),
        Err(e) => report.note(e),
    }
    report
}

/// `unicorn/prefer-module` flags every `require`/`module`/`__dirname`; one
/// finding per file (its first use on a changed line) says the same thing.
fn collapse_prefer_module(change: &Change, findings: &mut Vec<Finding>) {
    const RULE: &str = "unicorn/prefer-module";
    let mut per_file: std::collections::BTreeMap<String, (u32, u32, usize)> = std::collections::BTreeMap::new();
    for f in findings.iter().filter(|f| f.rule == RULE) {
        let on_change = change.files.get(&f.file).is_some_and(|l| l.contains(f.line));
        let e = per_file.entry(f.file.clone()).or_insert((0, 0, 0));
        e.2 += 1;
        if on_change && (e.0 == 0 || (f.line, f.column) < (e.0, e.1)) {
            e.0 = f.line;
            e.1 = f.column;
        }
    }
    findings.retain(|f| f.rule != RULE || per_file.get(&f.file).is_some_and(|&(l, c, _)| (f.line, f.column) == (l, c)));
    for f in findings.iter_mut().filter(|f| f.rule == RULE) {
        let uses = per_file.get(&f.file).map_or(1, |e| e.2);
        f.message =
            format!("CommonJS module ({uses} use(s) of require/module/exports/__dirname in this file): migrate the file to ES modules (ESM: import/export).");
    }
}

pub fn run_css(ctx: &Ctx, files: &[String]) -> StackReport {
    let mut report = StackReport::new("css", files.to_vec());
    report.newest("baseline", "newly");
    let Some(node) = driver_ready(ctx, &mut report, "stylelint") else { return report };
    let change = ctx.change;
    let mut entries = Vec::new();
    let mut pins_json = serde_json::Map::new();
    for f in files {
        let info = package_for(change, f);
        let available = pins::baseline_available(&info.browserslist);
        pins_json.insert(f.clone(), json!({"browserslist": info.browserslist, "available": available}));
        entries.push(json!({"path": f, "available": available}));
    }
    report.pin("files", Value::Object(pins_json));
    let dir = match ctx.temp.sub("css") {
        Ok(d) => d,
        Err(e) => {
            report.fail(e);
            return report;
        }
    };
    if let Err(e) = change.materialize(&dir, files) {
        report.fail(e);
        return report;
    }
    let job = json!({"mode": "stylelint", "root": dir.to_string_lossy(), "files": entries});
    let out = match run_driver(ctx, &node, &job, "stylelint", 120) {
        Ok(v) => v,
        Err(e) => {
            report.fail(e);
            return report;
        }
    };
    report.tool = out.get("tool").and_then(Value::as_str).unwrap_or("stylelint").to_string();
    for n in out.get("notes").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        report.note(n);
    }
    for r in out.get("results").and_then(Value::as_array).into_iter().flatten() {
        let Some(file) = r.get("file").and_then(Value::as_str).and_then(change::normalize) else { continue };
        let rule = r.get("rule").and_then(Value::as_str).unwrap_or("").to_string();
        let num = |k: &str| r.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
        let text = r.get("message").and_then(Value::as_str).unwrap_or("");
        let message = text.trim_suffix(format!(" ({rule})").as_str()).to_string();
        let (kind, confidence) = match rule.as_str() {
            "plugin/use-baseline" => (Kind::Baseline, Confidence::High),
            LONGHAND_RULE if !names_newer_shorthand(&message) => continue,
            LONGHAND_RULE => (Kind::Modern, Confidence::Low),
            _ => (Kind::Modern, Confidence::High),
        };
        report.findings.push(Finding {
            stack: "css",
            file,
            line: num("line"),
            column: num("column"),
            kind,
            rule,
            message,
            source: "tool",
            confidence,
            upgrade: None,
        });
    }
    report
}

/// stylelint's redundant-longhand rule. Its findings are kept only for
/// shorthands newer than their longhands, as `low`: the rest is style.
const LONGHAND_RULE: &str = "declaration-block-no-redundant-longhand-properties";
const NEWER_SHORTHANDS: [&str; 12] = [
    "inset",
    "inset-block",
    "inset-inline",
    "margin-block",
    "margin-inline",
    "padding-block",
    "padding-inline",
    "gap",
    "place-items",
    "place-content",
    "place-self",
    "overflow",
];

/// Whether a redundant-longhand message (`Expected shorthand property "inset"`)
/// names one of `NEWER_SHORTHANDS`.
fn names_newer_shorthand(message: &str) -> bool {
    message.split('"').nth(1).is_some_and(|name| NEWER_SHORTHANDS.contains(&name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esx_table_follows_the_engines_minimum() {
        let n22 = esx_rules_for((22, 0, 0));
        assert!(!n22.contains(&"es-x/no-set-prototype-union") && !n22.contains(&"es-x/no-array-fromasync"));
        assert!(!n22.contains(&"es-x/no-iterator-prototype-map") && n22.contains(&"es-x/no-promise-try"));
        let n16_9 = esx_rules_for((16, 9, 0));
        assert!(!n16_9.contains(&"es-x/no-object-hasown") && n16_9.contains(&"es-x/no-class-static-block"));
        assert!(esx_rules_for((18, 0, 0)).contains(&"es-x/no-regexp-v-flag"));
        assert!(!esx_rules_for((18, 0, 0)).contains(&"es-x/no-array-prototype-findlast-findlastindex"));
        assert_eq!(esx_rules_for((26, 0, 0)), Vec::<&str>::new());
    }

    #[test]
    fn esx_and_unicorn_tables_agree() {
        for (esx, rule) in SAME_FEATURE {
            let a = ES_X.iter().find(|(id, _)| *id == esx).map(|(_, v)| *v);
            let b = RULES.iter().find(|(id, ..)| *id == rule).map(|(_, v, ..)| *v);
            assert!(a.is_some() && a == b, "{esx} {a:?} vs {rule} {b:?}");
        }
    }

    #[test]
    fn style_only_rules_are_not_run() {
        assert!(!RULES.iter().any(|(id, ..)| *id == "prefer-const"));
        let eslint = include_str!("../../js/modern-tara.eslint.mjs");
        let core = eslint.lines().find(|l| l.starts_with("export const CORE")).unwrap();
        assert!(!core.contains("'prefer-const'") && core.contains("'no-var'"), "{core}");
        let stylelint = include_str!("../../js/modern-tara.stylelint.mjs");
        for dropped in ["'alpha-value-notation'", "'shorthand-property-no-redundant-values'"] {
            assert!(!stylelint.contains(dropped), "{dropped}");
        }
        assert!(stylelint.contains(&format!("'{LONGHAND_RULE}'")));
        assert!(stylelint.contains("'color-function-notation'") && stylelint.contains("'media-feature-range-notation'"));
    }

    #[test]
    fn redundant_longhands_count_only_for_newer_shorthands() {
        assert!(names_newer_shorthand("Expected shorthand property \"inset\""));
        assert!(names_newer_shorthand("Expected shorthand property \"place-items\""));
        assert!(!names_newer_shorthand("Expected shorthand property \"margin\""));
        assert!(!names_newer_shorthand("Expected shorthand property \"list-style\""));
        assert!(!names_newer_shorthand("Expected shorthand property"));
    }
}
