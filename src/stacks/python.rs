//! Python: ruff's stable UP, FURB, PTH and SIM rules, and the families that
//! turn long code into a shorter construct (`SHORTENING`), at the project's
//! `requires-python`, then at 3.15; findings only the newer targets show are
//! `needs-newer`, with the first version that enables them.

use super::{Ctx, astgrep};
use crate::change::{self, Change};
use crate::pins::{self, NEWEST_PYTHON};
use crate::proc::Cmd;
use crate::report::{Confidence, Finding, Kind, StackReport, Upgrade};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// E902 (io-error) is selected so a file ruff cannot read is reported, not skipped.
pub const SELECT: &str = "UP,FURB,PTH,SIM,PERF101,PERF102,PERF401,PERF403,C4,RUF005,RUF015,RUF017,PLR1714,PLR1730,PLR1733,PLR1736,FLY002,E902";
/// The selectors of the shortening families inside `SELECT`. Their findings
/// are `high` only when ruff's own fix is `safe`, else `low`.
const SHORTENING: [&str; 13] =
    ["PERF101", "PERF102", "PERF401", "PERF403", "C4", "RUF005", "RUF015", "RUF017", "PLR1714", "PLR1730", "PLR1733", "PLR1736", "FLY002"];
/// Oldest target ruff accepts.
const RUFF_MIN: (u32, u32) = (3, 7);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Pin {
    /// Where it came from: `pyproject.toml path`, `.python-version path`, or "".
    source: String,
    spec: String,
    min: Option<(u32, u32)>,
}

#[expect(clippy::too_many_lines, reason = "one linear pipeline: pins, copy, run, parse")]
pub fn run(ctx: &Ctx, files: &[String]) -> StackReport {
    let mut report = StackReport::new("python", files.to_vec());
    report.newest("python", format!("{}.{}", NEWEST_PYTHON.0, NEWEST_PYTHON.1));
    let Some(ruff) = ctx.tools.ruff.clone() else {
        report.skip("ruff not found: install it with scripts/install-tools.ps1 (Windows) or scripts/install-tools.sh (Linux).");
        return report;
    };
    report.tool = super::version_line(&ruff, &["--version"]).unwrap_or_else(|| "ruff".into());
    report.ran(SELECT.split(','));

    let mut groups: BTreeMap<Pin, Vec<String>> = BTreeMap::new();
    for f in files {
        groups.entry(pin_for(ctx.change, f)).or_default().push(f.clone());
    }
    let mut pin_json = serde_json::Map::new();
    for (pin, members) in &groups {
        let label = if pin.source.is_empty() { "unknown (assumed newest)".to_string() } else { format!("{} ({})", pin.spec, pin.source) };
        for m in members {
            pin_json.insert(m.clone(), Value::String(label.clone()));
        }
    }
    report.pin("requires-python", Value::Object(pin_json));

    let copy = match ctx.temp.sub("python") {
        Ok(d) => d,
        Err(e) => {
            report.fail(e);
            return report;
        }
    };
    if let Err(e) = ctx.change.materialize(&copy, files) {
        report.fail(e);
        return report;
    }

    for (pin, all) in &groups {
        // A file too large or unreadable to copy is unscanned by the caller, not a ruff error.
        let members = &all.iter().filter(|m| copy.join(m).is_file()).cloned().collect::<Vec<_>>();
        let base = pin.min.map_or(NEWEST_PYTHON, |v| v.max(RUFF_MIN).min(NEWEST_PYTHON));
        let base_hits = match ruff_run(ctx, &ruff, &copy, members, base) {
            Ok((h, broken)) => {
                for (file, why) in broken {
                    report.unscanned(file, why);
                }
                h
            }
            Err(e) => {
                report.fail(e);
                return report;
            }
        };
        let base_keys: HashSet<(String, u32, String)> = base_hits.iter().map(Hit::key).collect();
        for h in &base_hits {
            report.findings.push(h.finding(Kind::Modern, None));
        }
        if base < NEWEST_PYTHON {
            let newest_hits = match ruff_run(ctx, &ruff, &copy, members, NEWEST_PYTHON) {
                Ok((h, _)) => h,
                Err(e) => {
                    report.note(format!("needs-newer pass failed: {e}"));
                    continue;
                }
            };
            let extra: Vec<Hit> = newest_hits.into_iter().filter(|h| !base_keys.contains(&h.key())).collect();
            if extra.is_empty() {
                continue;
            }
            // The first target that shows each extra finding is its upgrade.
            let mut first_seen: HashMap<(String, u32, String), (u32, u32)> = HashMap::new();
            for minor in base.1 + 1..NEWEST_PYTHON.1 {
                if let Ok((hits, _)) = ruff_run(ctx, &ruff, &copy, members, (3, minor)) {
                    for h in hits {
                        first_seen.entry(h.key()).or_insert((3, minor));
                    }
                }
            }
            let pin_name = if pin.source.ends_with(".python-version") { ".python-version" } else { "requires-python" };
            for h in &extra {
                let to = first_seen.get(&h.key()).copied().unwrap_or(NEWEST_PYTHON);
                let upgrade = Upgrade {
                    pin: pin_name.into(),
                    from: pin.spec.clone(),
                    to: format!(">={}.{}", to.0, to.1),
                    note: format!("ruff reports it from target py{}{}; newest stable Python is {}.{}", to.0, to.1, NEWEST_PYTHON.0, NEWEST_PYTHON.1),
                };
                report.findings.push(h.finding(Kind::NeedsNewer, Some(upgrade)));
            }
        }
    }

    match astgrep::scan(ctx, &copy, files) {
        Ok(Some(matches)) => {
            report.ran(astgrep::rule_ids(&ctx.home, "requires-python"));
            for m in matches.into_iter().filter(|m| m.gate == "requires-python") {
                let pin = pin_for(ctx.change, &m.file);
                let min = pins::major_minor(&m.min);
                let below = matches!((pin.min, min), (Some(have), Some(need)) if have < need);
                let note = format!("newest stable Python is {}.{}", NEWEST_PYTHON.0, NEWEST_PYTHON.1);
                let mut f = astgrep::to_finding("python", m, Some(&pin.spec), below, &note);
                if let Some(u) = &mut f.upgrade {
                    u.to = format!(">={}", u.to);
                }
                report.findings.push(f);
            }
        }
        Ok(None) => report.note("ast-grep not found: catalogue rules skipped."),
        Err(e) => report.fail(e),
    }
    report
}

struct Hit {
    file: String,
    line: u32,
    column: u32,
    code: String,
    message: String,
    confidence: Confidence,
}

impl Hit {
    fn key(&self) -> (String, u32, String) {
        (self.file.clone(), self.line, self.code.clone())
    }
    fn finding(&self, kind: Kind, upgrade: Option<Upgrade>) -> Finding {
        Finding {
            stack: "python",
            file: self.file.clone(),
            line: self.line,
            column: self.column,
            rule: self.code.clone(),
            message: self.message.clone(),
            kind,
            source: "tool",
            confidence: self.confidence,
            upgrade,
        }
    }
}

/// The hits, and the files ruff could not parse or read (with the reason).
/// `--ignore-noqa`: a `# noqa` comment never hides a finding (only
/// modern-tara.toml waives).
fn ruff_run(ctx: &Ctx, ruff: &Path, copy: &Path, files: &[String], target: (u32, u32)) -> Result<(Vec<Hit>, BTreeMap<String, String>), String> {
    let mut hits = Vec::new();
    let mut broken = BTreeMap::new();
    let cmd = Cmd::new(ruff)
        .args(["check", "--no-cache", "--isolated", "--exit-zero", "--ignore-noqa", "--select", SELECT, "--output-format", "json"])
        .arg("--target-version")
        .arg(format!("py{}{}", target.0, target.1))
        .arg("--")
        .cwd(copy)
        .env("NO_COLOR", "1")
        .timeout(ctx.timeout_or(120));
    for out in cmd.run_chunked(files)? {
        if out.timed_out {
            return Err("ruff timed out".into());
        }
        if !out.ok() {
            return Err(format!("ruff failed: {}", out.tail(3)));
        }
        for (file, why) in lint_failures(&out.stderr_text(), files) {
            broken.entry(file).or_insert_with(|| format!("ruff could not read it: {why}"));
        }
        let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("ruff output: {e}"))?;
        for item in v.as_array().into_iter().flatten() {
            let Some(file) = item.get("filename").and_then(Value::as_str).and_then(|p| super::rel_to(copy, p)) else { continue };
            let num = |ptr: &str| item.pointer(ptr).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
            let Some(code) = item.get("code").and_then(Value::as_str) else { continue };
            let message = item.get("message").and_then(Value::as_str).unwrap_or("");
            if code == "invalid-syntax" {
                broken.entry(file).or_insert_with(|| format!("ruff could not parse it: {message} (line {})", num("/location/row")));
                continue;
            }
            if code == "E902" {
                broken.entry(file).or_insert_with(|| format!("ruff could not read it: {message}"));
                continue;
            }
            let safe_fix = item.pointer("/fix/applicability").and_then(Value::as_str) == Some("safe");
            let confidence = confidence_for(code, safe_fix);
            hits.push(Hit {
                file,
                line: num("/location/row"),
                column: num("/location/column"),
                code: code.to_string(),
                message: message.to_string(),
                confidence,
            });
        }
    }
    Ok((hits, broken))
}

/// `high` for every code but the shortening families, which are `high`
/// only with a safe fix: ruff's other fixes may change behaviour.
fn confidence_for(code: &str, safe_fix: bool) -> Confidence {
    if safe_fix || !SHORTENING.iter().any(|selector| code.starts_with(selector)) { Confidence::High } else { Confidence::Low }
}

/// `(file, why)` of each `Failed to lint <file>: <why>` line on ruff's stderr
/// that names one of `files`.
fn lint_failures(stderr: &str, files: &[String]) -> Vec<(String, String)> {
    let failures = stderr.lines().filter_map(|l| l.split_once("Failed to lint ")).map(|(_, rest)| rest.replace('\\', "/"));
    failures.filter_map(|f| files.iter().find_map(|file| f.strip_prefix(&format!("{file}: ")).map(|why| (file.clone(), why.to_string())))).collect()
}

/// Nearest `pyproject.toml` with `requires-python`, else `.python-version`.
fn pin_for(change: &Change, file: &str) -> Pin {
    for dir in change::ancestors(change::parent(file)) {
        let pyproject = change::join(&dir, "pyproject.toml");
        if change.exists(&pyproject)
            && let Some(text) = change.read_text(&pyproject)
            && let Ok(table) = text.parse::<toml::Table>()
            && let Some(spec) = table.get("project").and_then(|p| p.get("requires-python")).and_then(|v| v.as_str())
        {
            return Pin { source: pyproject, spec: spec.to_string(), min: pins::requires_python_min(spec) };
        }
        let pv = change::join(&dir, ".python-version");
        if change.exists(&pv)
            && let Some(text) = change.read_text(&pv)
        {
            let min = pins::python_version_file(&text);
            return Pin { source: pv, spec: text.lines().next().unwrap_or("").trim().to_string(), min };
        }
    }
    Pin { source: String::new(), spec: "unknown".into(), min: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortening_families_are_selected_and_high_only_with_a_safe_fix() {
        let selected: Vec<&str> = SELECT.split(',').collect();
        assert!(SHORTENING.iter().all(|s| selected.contains(s)), "{selected:?}");
        assert_eq!(confidence_for("PLR1736", true), Confidence::High);
        assert_eq!(confidence_for("PLR1736", false), Confidence::Low);
        assert_eq!(confidence_for("C408", false), Confidence::Low);
        assert_eq!(confidence_for("C408", true), Confidence::High);
        // The older families are never downgraded.
        assert_eq!(confidence_for("UP006", false), Confidence::High);
        assert_eq!(confidence_for("PTH118", false), Confidence::High);
        assert_eq!(confidence_for("SIM108", false), Confidence::High);
    }

    #[test]
    fn lint_failures_name_the_file() {
        let files = ["a.py".to_string(), "a.pyi".to_string(), "src/b.py".to_string()];
        let stderr = "warning: Failed to lint src\\b.py: stream did not contain valid UTF-8\nwarning: Failed to lint other.py: x\nwarning: Failed to lint a.pyi: y\nunrelated\n";
        let named = |f: &str, why: &str| (f.to_string(), why.to_string());
        assert_eq!(lint_failures(stderr, &files), vec![named("src/b.py", "stream did not contain valid UTF-8"), named("a.pyi", "y")]);
    }
}
