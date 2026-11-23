//! Scanner-only regression test over the `modern-bench` fixtures (skills repo,
//! `evals/modern-bench`). Not part of CI: the bench lives outside this repository.
//!
//! ```text
//! cargo test --locked --test bench -- --ignored --nocapture
//! ```
//!
//! `MODERN_BENCH` overrides the bench folder (default
//! `~/.claude/skills/proje-zinciri/evals/modern-bench`). `MODERN_TARA_HOME`, when
//! set, replaces this checkout as the scanner home: the js and css stacks need a
//! home whose `js/` has `node_modules` (an install). Per stack the fixture is
//! copied to a fresh temp folder (no `.git`) and scanned with `--all-lines`
//! alone (every file); then `expected/<stack>.md` is checked:
//!
//! - a row tagged `tool:<rule>` needs a finding of that rule inside its line range;
//! - no high-confidence `modern` finding may sit on a decoy line range;
//! - a stack that is skipped, or reports an `unscanned` file, fails;
//! - `needs-newer:` and `catalogue` rows are shown, never asserted.
//!
//! Decoy bullets that name only a file (no `:line`) are skipped: none of them says
//! the whole file is current, they describe one construct without a line.
//! High-confidence `modern` findings on lines that are neither planted nor decoy
//! are listed as unplanned hits; they do not fail the test.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const STACKS: [&str; 7] = ["csharp", "rust", "python", "js", "css", "powershell", "go"];

/// Inclusive line range.
type Lines = (u32, u32);

struct Row {
    id: String,
    file: String,
    /// The lines a finding must hit (the text before any parenthesis).
    asserted: Vec<Lines>,
    /// Every line the cell names, parenthetical notes included.
    planted: Vec<Lines>,
    caught_by: String,
}

struct Decoy {
    file: String,
    lines: Vec<Lines>,
}

struct Found {
    file: String,
    line: u32,
    rule: String,
    kind: String,
    confidence: String,
}

impl Found {
    fn high_modern(&self) -> bool {
        self.kind == "modern" && self.confidence == "high"
    }
}

struct Outcome {
    tool_rows: usize,
    caught: usize,
    decoy_hits: usize,
    unplanned: usize,
}

fn bench_dir() -> PathBuf {
    std::env::var_os("MODERN_BENCH").map_or_else(
        || std::env::home_dir().unwrap_or_default().join(".claude").join("skills").join("proje-zinciri").join("evals").join("modern-bench"),
        PathBuf::from,
    )
}

/// Rules and `js/` of the scanner: `MODERN_TARA_HOME` when set (an install whose
/// `js/` holds `node_modules`), else this checkout.
fn scanner_home() -> PathBuf {
    std::env::var_os("MODERN_TARA_HOME").filter(|v| !v.is_empty()).map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from)
}

/// `10`, `10-13`, `4, 6-10`, `flag at 9, 19`: every number or range in the text.
fn parse_lines(text: &str) -> Vec<Lines> {
    text.split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter_map(|token| match token.split_once('-') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => token.parse().ok().map(|n| (n, n)),
        })
        .collect()
}

fn in_lines(ranges: &[Lines], line: u32) -> bool {
    ranges.iter().any(|&(a, b)| (a..=b).contains(&line))
}

fn same_file(found: &str, want: &str) -> bool {
    let found = found.replace('\\', "/");
    found == want || found.ends_with(&format!("/{want}"))
}

/// Case-insensitive; `clippy::x` matches `x`, `go/any` matches `any`, `ruff:UP006` matches `UP006`.
fn rule_matches(found: &str, want: &str) -> bool {
    let (f, w) = (found.to_lowercase(), want.to_lowercase());
    let tail = |long: &str, short: &str| long.strip_suffix(short).is_some_and(|head| head.ends_with([':', '/']));
    f == w || tail(&f, &w) || tail(&w, &f)
}

/// The rules of a `tool:` tag: `tool:IDE0300 and CA1825 (seen)` names two.
fn tool_rules(caught_by: &str) -> Vec<String> {
    let tag = caught_by.strip_prefix("tool:").unwrap_or_default();
    let tag = tag.split('(').next().unwrap_or_default();
    tag.split(" and ").flat_map(|part| part.split(" or ")).filter_map(|part| part.split_whitespace().next()).map(str::to_string).collect()
}

fn parse_row(line: &str) -> Option<Row> {
    let cells: Vec<String> = line.replace("\\|", "\u{1}").split('|').map(|c| c.replace('\u{1}', "|").trim().to_string()).collect();
    let [_, id, place, .., caught_by, _] = cells.as_slice() else { return None };
    if !id.starts_with(|c: char| c.is_ascii_uppercase()) || !id.ends_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let (file, rest) = place.split_once(':')?;
    let asserted = parse_lines(rest.split('(').next().unwrap_or_default());
    Some(Row { id: id.clone(), file: file.trim().to_string(), asserted, planted: parse_lines(rest), caught_by: caught_by.clone() })
}

/// `- src/a.rs:53-56: text` and `- src/a.py:1 and b.py:2: text`; bullets without a line are skipped.
fn parse_decoy(bullet: &str) -> Vec<Decoy> {
    let lead = bullet.trim_start_matches("- ").split_once(": ").map_or("", |(lead, _)| lead);
    lead.split(" and ")
        .filter_map(|part| {
            let (file, rest) = part.rsplit_once(':')?;
            let lines = parse_lines(rest);
            let plain = !file.contains([' ', '`']) && rest.chars().all(|c| c.is_ascii_digit() || " -,".contains(c));
            (plain && !lines.is_empty()).then(|| Decoy { file: file.to_string(), lines })
        })
        .collect()
}

fn parse_expected(text: &str) -> (Vec<Row>, Vec<Decoy>) {
    let mut in_decoys = false;
    let (mut rows, mut decoys) = (Vec::new(), Vec::new());
    for line in text.lines() {
        if line.starts_with('#') {
            in_decoys = line.contains("Decoys");
        } else if in_decoys && line.starts_with("- ") {
            decoys.extend(parse_decoy(line));
        } else if line.starts_with('|') {
            rows.extend(parse_row(line));
        }
    }
    (rows, decoys)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A scratch folder that is removed with the guard.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Scans a fresh copy of the fixture; the parsed JSON report.
fn scan(stack: &str, fixture: &Path) -> Result<Value, String> {
    let root = Scratch(std::env::temp_dir().join(format!("mb-run-{}-{stack}", std::process::id())));
    let _ = fs::remove_dir_all(&root.0);
    let copy = root.0.join("work");
    copy_tree(fixture, &copy);
    let json = root.0.join("report.json");
    let out = Command::new(env!("CARGO_BIN_EXE_modern-tara"))
        .arg("scan")
        .arg("--repo")
        .arg(&copy)
        .args(["--all-lines", "--json"])
        .arg(&json)
        .arg("--quiet")
        .env("MODERN_TARA_CACHE", root.0.join("cache"))
        .env("MODERN_TARA_HOME", scanner_home())
        .output()
        .map_err(|e| format!("cannot run modern-tara: {e}"))?;
    let text = fs::read_to_string(&json).map_err(|e| format!("no report ({e}): {}", String::from_utf8_lossy(&out.stderr)))?;
    serde_json::from_str(&text).map_err(|e| format!("bad report JSON: {e}"))
}

/// The report's entry for the stack.
fn stack_entry<'a>(report: &'a Value, stack: &str) -> Option<&'a Value> {
    report["stacks"].as_array()?.iter().find(|s| s["stack"] == stack)
}

/// Why the stack did not run (skipped: tool missing, or error), if so.
fn not_run(report: &Value, stack: &str) -> Option<String> {
    let Some(entry) = stack_entry(report, stack) else { return Some("the report has no entry for this stack".into()) };
    let status = entry["status"].as_str().unwrap_or("?");
    (status != "ok").then(|| format!("status {status}: {}", entry["message"].as_str().unwrap_or("")))
}

/// `file: reason` of every file the stack left unscanned.
fn unscanned(report: &Value, stack: &str) -> Vec<String> {
    let list = stack_entry(report, stack).and_then(|e| e["unscanned"].as_array()).map_or(&[][..], Vec::as_slice);
    list.iter().map(|u| format!("{}: {}", u["file"].as_str().unwrap_or("?"), u["reason"].as_str().unwrap_or(""))).collect()
}

fn findings(report: &Value) -> Vec<Found> {
    let text = |f: &Value, k: &str| f[k].as_str().unwrap_or_default().to_string();
    let list = report["findings"].as_array().map_or(&[][..], Vec::as_slice);
    list.iter()
        .map(|f| Found {
            file: text(f, "file").replace('\\', "/"),
            line: f["line"].as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0),
            rule: text(f, "rule"),
            kind: text(f, "kind"),
            confidence: text(f, "confidence"),
        })
        .collect()
}

/// One table line per row; returns `(tool rows, caught)`.
fn check_rows(rows: &[Row], found: &[Found], failures: &mut Vec<String>, stack: &str) -> (usize, usize) {
    let (mut tool_rows, mut caught) = (0, 0);
    for row in rows {
        let at = |f: &&Found| same_file(&f.file, &row.file) && in_lines(&row.asserted, f.line);
        let result = if row.caught_by.starts_with("tool:") {
            tool_rows += 1;
            let rules = tool_rules(&row.caught_by);
            if let Some(hit) = found.iter().filter(at).find(|f| rules.iter().any(|r| rule_matches(&f.rule, r))) {
                caught += 1;
                format!("caught: {} at line {}", hit.rule, hit.line)
            } else {
                failures.push(format!("{stack} {}: no {} finding in {} lines {:?}", row.id, rules.join("/"), row.file, row.asserted));
                let near: Vec<String> = found.iter().filter(at).map(|f| format!("{}@{}", f.rule, f.line)).collect();
                format!("MISSED (findings there: {})", near.join(", "))
            }
        } else if row.caught_by.starts_with("needs-newer:") {
            let seen = found.iter().filter(at).any(|f| f.kind == "needs-newer");
            format!("not asserted, needs-newer {}", if seen { "reported" } else { "not reported" })
        } else {
            "not asserted (catalogue)".to_string()
        };
        println!("  {:<4} {:<46} {result}", row.id, row.caught_by.chars().take(46).collect::<String>());
    }
    (tool_rows, caught)
}

/// High-confidence `modern` findings on decoy lines (failures), and on lines nobody planned.
fn check_lines(rows: &[Row], decoys: &[Decoy], found: &[Found], failures: &mut Vec<String>, stack: &str) -> (usize, usize) {
    let (mut hits, mut unplanned) = (0, 0);
    for f in found.iter().filter(|f| f.high_modern()) {
        let on_decoy = decoys.iter().any(|d| same_file(&f.file, &d.file) && in_lines(&d.lines, f.line));
        let planted = rows.iter().any(|r| same_file(&f.file, &r.file) && in_lines(&r.planted, f.line));
        if on_decoy {
            hits += 1;
            println!("  DECOY HIT   {}:{} {}", f.file, f.line, f.rule);
            failures.push(format!("{stack} decoy flagged: {}:{} {}", f.file, f.line, f.rule));
        } else if !planted {
            unplanned += 1;
            println!("  unplanned   {}:{} {}", f.file, f.line, f.rule);
        }
    }
    (hits, unplanned)
}

fn run_stack(bench: &Path, stack: &str, failures: &mut Vec<String>) -> Option<Outcome> {
    let (fixture, expected) = (bench.join("fixtures").join(stack), bench.join("expected").join(format!("{stack}.md")));
    println!("== {stack}");
    if !fixture.is_dir() || !expected.is_file() {
        println!("  no fixture or expected file, left out");
        return None;
    }
    let (rows, decoys) = parse_expected(&fs::read_to_string(&expected).unwrap());
    let report = match scan(stack, &fixture) {
        Ok(r) => r,
        Err(e) => {
            failures.push(format!("{stack}: {e}"));
            println!("  FAILED: {e}");
            return None;
        }
    };
    if let Some(why) = not_run(&report, stack) {
        failures.push(format!("{stack}: the scanner did not run it ({why})"));
        println!("  NOT RUN: {why}");
        return None;
    }
    for file in unscanned(&report, stack) {
        failures.push(format!("{stack}: unscanned {file}"));
        println!("  UNSCANNED   {file}");
    }
    let found = findings(&report);
    let (tool_rows, caught) = check_rows(&rows, &found, failures, stack);
    let (decoy_hits, unplanned) = check_lines(&rows, &decoys, &found, failures, stack);
    Some(Outcome { tool_rows, caught, decoy_hits, unplanned })
}

#[test]
#[ignore = "needs the modern-bench folder and the stack tools; run with --ignored"]
fn bench_fixtures() {
    let bench = bench_dir();
    assert!(bench.join("fixtures").is_dir(), "no bench at {} (set MODERN_BENCH)", bench.display());
    let mut failures = Vec::new();
    let mut summary = Vec::new();
    for stack in STACKS {
        if let Some(o) = run_stack(&bench, stack, &mut failures) {
            summary.push((stack, o));
        }
    }
    println!("\n{:<11} {:>12} {:>11} {:>10}", "stack", "tool caught", "decoy hits", "unplanned");
    for (stack, o) in &summary {
        println!("{stack:<11} {:>9}/{:<2} {:>11} {:>10}", o.caught, o.tool_rows, o.decoy_hits, o.unplanned);
    }
    assert!(failures.is_empty(), "{} failure(s):\n{}", failures.len(), failures.join("\n"));
}
