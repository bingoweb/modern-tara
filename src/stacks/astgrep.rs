//! Catalogue rules no linter has, as ast-grep YAML under `rules/`. Each rule's
//! `metadata` says which pin gates it: `gate` (`rust-version`,
//! `requires-python`, `engines.node`, `TargetFramework`) and `min` (the first
//! release with the modern construct). `confidence` (`high` or `low`) says
//! whether the pattern is exact or a heuristic; `--gate` stops only on `high`.

use super::Ctx;
use crate::proc::Cmd;
use crate::report::{Confidence, Finding, Kind, Upgrade};
use serde_json::Value;
use std::path::Path;

pub struct Match {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub rule: String,
    pub message: String,
    pub gate: String,
    pub min: String,
    pub confidence: Confidence,
}

/// Runs every rule over `files` (relative to `copy`; files the copy lacks
/// are left out, the caller reports them). `Err` when ast-grep exists but
/// failed (see [`check`]); a missing ast-grep gives `Ok(None)`.
pub fn scan(ctx: &Ctx, copy: &Path, files: &[String]) -> Result<Option<Vec<Match>>, String> {
    let Some(exe) = &ctx.tools.ast_grep else { return Ok(None) };
    let files: Vec<&String> = files.iter().filter(|f| copy.join(f).is_file()).collect();
    if files.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let config = ctx.home.join("sgconfig.yml");
    if !config.is_file() {
        return Err(format!("{} is missing", config.display()));
    }
    let cmd = Cmd::new(exe)
        .arg("scan")
        .arg("--config")
        .arg(&config)
        .args(["--json=stream", "--include-metadata", "--color", "never"])
        .cwd(copy)
        .timeout(ctx.timeout_or(60));
    let mut matches = Vec::new();
    for out in cmd.run_chunked(&files)? {
        if out.timed_out {
            return Err("ast-grep timed out".into());
        }
        check(&out)?;
        for line in out.stdout_text().lines().filter(|l| l.starts_with('{')) {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if let Some(m) = parse_match(&v) {
                matches.push(m);
            }
        }
    }
    Ok(Some(matches))
}

/// An ast-grep run is clean only with exit 0, or exit 1 for error-severity
/// matches it printed, and no `Error:`/`ERROR:` line on stderr (a broken
/// rule file exits 8; a file it cannot read is an `ERROR:` line with exit 0).
fn check(out: &crate::proc::Output) -> Result<(), String> {
    let stderr = out.stderr_text();
    let found_in_code = stderr.contains("error(s) found in code");
    let has_matches = out.stdout_text().lines().any(|l| serde_json::from_str::<Value>(l).is_ok_and(|v| v.get("ruleId").is_some()));
    if let Some(line) = stderr.lines().map(str::trim).find(|l| (l.starts_with("Error:") || l.starts_with("ERROR:")) && !l.contains("error(s) found in code")) {
        return Err(format!("ast-grep failed: {}", line.chars().take(300).collect::<String>()));
    }
    match out.code {
        Some(0) => Ok(()),
        Some(1) if found_in_code && has_matches => Ok(()),
        code => Err(format!("ast-grep failed (exit {code:?}): {}", out.tail(2))),
    }
}

/// Ids of the catalogue rules (`<home>/rules/*.yml`) whose `metadata.gate`
/// is `gate`: the rules a stack keeps from the scan.
pub fn rule_ids(home: &Path, gate: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let Ok(entries) = std::fs::read_dir(home.join("rules")) else { return ids };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "yml" || e == "yaml")).collect();
    paths.sort();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for doc in text.split("\n---") {
            let field = |key: &str| doc.lines().find_map(|l| l.trim().strip_prefix(key)).map(|v| v.trim().trim_matches('"').to_string());
            if let Some(id) = doc.lines().find_map(|l| l.strip_prefix("id:")).map(|v| v.trim().to_string())
                && field("gate:").as_deref() == Some(gate)
            {
                ids.push(id);
            }
        }
    }
    ids
}

fn parse_match(v: &Value) -> Option<Match> {
    let file = crate::change::normalize(v.get("file")?.as_str()?)?;
    let start = v.pointer("/range/start")?;
    let meta = v.get("metadata");
    let text = |key: &str| meta.and_then(|m| m.get(key)).and_then(Value::as_str).unwrap_or("").to_string();
    Some(Match {
        file,
        line: u32::try_from(start.get("line")?.as_u64()? + 1).ok()?,
        column: u32::try_from(start.get("column")?.as_u64()? + 1).ok()?,
        rule: v.get("ruleId")?.as_str()?.to_string(),
        message: v.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
        gate: text("gate"),
        min: text("min"),
        confidence: Confidence::parse(&text("confidence")),
    })
}

/// A match as a finding: `needs-newer` when `below_min` says the pin is
/// older than the rule's `min`.
pub fn to_finding(stack: &'static str, m: Match, pin_value: Option<&str>, below_min: bool, note: &str) -> Finding {
    let upgrade =
        below_min.then(|| Upgrade { pin: m.gate.clone(), from: pin_value.unwrap_or("unknown").to_string(), to: m.min.clone(), note: note.to_string() });
    Finding {
        stack,
        file: m.file,
        line: m.line,
        column: m.column,
        rule: m.rule,
        message: m.message,
        kind: if below_min { Kind::NeedsNewer } else { Kind::Modern },
        source: "ast-grep",
        confidence: m.confidence,
        upgrade,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(id, metadata.confidence)` of every rule in `rules/*.yml`.
    fn rule_confidences() -> Vec<(String, String)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules");
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap();
            for doc in text.split("\n---") {
                let field = |key: &str| doc.lines().find_map(|l| l.trim().strip_prefix(key)).map(|v| v.trim().trim_matches('"').to_string());
                if let Some(id) = doc.lines().find_map(|l| l.strip_prefix("id: ")) {
                    out.push((id.trim().to_string(), field("confidence:").unwrap_or_default()));
                }
            }
        }
        out
    }

    #[test]
    fn every_rule_names_its_confidence_and_heuristics_are_low() {
        let rules = rule_confidences();
        assert!(rules.len() >= 9, "{rules:?}");
        for (id, c) in &rules {
            assert!(c == "high" || c == "low", "{id}: metadata.confidence is {c:?}");
        }
        let low = ["csharp-datetime-now", "csharp-httpclient-per-call", "csharp-new-random", "python-asyncio-gather", "javascript-http-client-package"];
        for id in low {
            assert!(rules.iter().any(|(r, c)| r == id && c == "low"), "{id} must be low");
        }
        assert!(rules.iter().any(|(r, c)| r == "rust-oncelock-static-lazylock" && c == "high"));
    }

    fn output(code: i32, stdout: &str, stderr: &str) -> crate::proc::Output {
        crate::proc::Output { code: Some(code), stdout: stdout.into(), stderr: stderr.into(), ..Default::default() }
    }

    #[test]
    fn ast_grep_errors_are_never_clean() {
        let line = r#"{"file":"a.rs","range":{"start":{"line":0,"column":0}},"ruleId":"x","message":"m"}"#;
        assert!(check(&output(0, "", "")).is_ok());
        assert!(check(&output(0, line, "")).is_ok());
        assert!(check(&output(1, line, "Error: 1 error(s) found in code.\nHelp: Scan succeeded and found error level diagnostics in the codebase.\n")).is_ok());
        let broken = check(&output(8, "", "Error: Cannot parse rule rules/x.yml\nHelp: The file is not a valid ast-grep rule.")).unwrap_err();
        assert!(broken.contains("Cannot parse rule"), "{broken}");
        let missing = check(&output(0, line, "ERROR: b.rs: The system cannot find the file specified. (os error 2)\n")).unwrap_err();
        assert!(missing.contains("b.rs"), "{missing}");
        assert!(check(&output(1, "", "")).is_err(), "exit 1 without matches");
        assert!(check(&output(2, "garbage", "")).is_err());
    }

    #[test]
    fn rule_ids_follow_the_gate() {
        let home = Path::new(env!("CARGO_MANIFEST_DIR"));
        let rust = rule_ids(home, "rust-version");
        assert!(rust.contains(&"rust-oncelock-static-lazylock".to_string()) && rust.contains(&"rust-strip-unwrap-or-trim".to_string()), "{rust:?}");
        assert!(!rust.iter().any(|r| r.starts_with("csharp-") || r.starts_with("go-")), "{rust:?}");
        let go = rule_ids(home, "go");
        assert!(go.contains(&"go-ioutil-io-os".to_string()) && go.iter().all(|r| r.starts_with("go-")), "{go:?}");
        assert!(rule_ids(home, "engines.node").contains(&"javascript-http-client-package".to_string()));
    }

    /// `(rule, 1-based line)` of the repository's catalogue on one Rust file.
    fn rust_matches(code: &str) -> Option<Vec<(String, usize)>> {
        let exe =
            crate::proc::which("ast-grep", &[std::env::home_dir()?.join(".cargo").join("bin").join(format!("ast-grep{}", std::env::consts::EXE_SUFFIX))])?;
        let dir = std::env::temp_dir().join(format!("mt-sg-rust-{}-{}", std::process::id(), code.len()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), code).unwrap();
        let home = Path::new(env!("CARGO_MANIFEST_DIR"));
        let out = Cmd::new(exe).arg("scan").arg("--config").arg(home.join("sgconfig.yml")).args(["--json=stream", "a.rs"]).cwd(&dir).run().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let mut found: Vec<(String, usize)> = out
            .stdout_text()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|v| (v["ruleId"].as_str().unwrap().to_string(), usize::try_from(v["range"]["start"]["line"].as_u64().unwrap()).unwrap() + 1))
            .collect();
        found.sort();
        Some(found)
    }

    #[test]
    fn oncelock_rule_skips_runtime_set_statics() {
        let code = "use std::sync::OnceLock;
static INIT_ONLY: OnceLock<String> = OnceLock::new();
static RUNTIME: OnceLock<String> = OnceLock::new();
static PATHED: OnceLock<u32> = OnceLock::new();
static UNUSED: OnceLock<u32> = OnceLock::new();
mod inner {
    pub static NESTED: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    pub fn n() -> u32 { *NESTED.get_or_init(|| 1) }
}
pub fn init_only() -> &'static str { INIT_ONLY.get_or_init(|| \"x\".to_string()) }
pub fn configure(v: String) { let _ = RUNTIME.set(v); }
pub fn runtime() -> &'static str { RUNTIME.get_or_init(String::new) }
pub fn pathed() { let _ = crate::PATHED.set(2); PATHED.get_or_init(|| 1); }
pub fn local() -> u32 { static LOCAL: OnceLock<u32> = OnceLock::new(); *LOCAL.get_or_init(|| 2) }
pub fn cell() { let c = std::cell::Cell::new(1); c.set(2); }
";
        let Some(found) = rust_matches(code) else { return };
        let lazy: Vec<usize> = found.iter().filter(|(r, _)| r == "rust-oncelock-static-lazylock").map(|(_, l)| *l).collect();
        assert_eq!(lazy, vec![2, 7, 14], "get_or_init only; never a static that is also .set(): {found:?}");
    }

    /// A `get_or_init` in a function with parameters may capture them, which
    /// `LazyLock` cannot: low confidence (`-captures`); parameterless stays high.
    #[test]
    fn oncelock_rule_is_low_when_the_closure_may_capture_parameters() {
        let code = "use std::sync::OnceLock;
static PLAIN: OnceLock<u32> = OnceLock::new();
static CAPTURING: OnceLock<u32> = OnceLock::new();
static METHOD: OnceLock<u32> = OnceLock::new();
pub fn plain() -> u32 { *PLAIN.get_or_init(|| 1) }
pub fn capturing(n: u32) -> u32 { *CAPTURING.get_or_init(|| n) }
pub struct S(u32);
impl S {
    pub fn method(&self) -> u32 { *METHOD.get_or_init(|| self.0) }
}
";
        let Some(found) = rust_matches(code) else { return };
        let lines = |id: &str| -> Vec<usize> { found.iter().filter(|(r, _)| r == id).map(|(_, l)| *l).collect() };
        assert_eq!(lines("rust-oncelock-static-lazylock"), vec![2], "{found:?}");
        assert_eq!(lines("rust-oncelock-static-lazylock-captures"), vec![3, 4], "{found:?}");
    }

    #[test]
    fn duration_rule_is_only_from_secs_times_60_or_3600() {
        let code = "use std::time::Duration;
pub fn f(x: u64, y: u64) -> u64 {
    let a = x * 60;
    let b = 60 * y;
    let _ = Duration::from_secs(x * 60);
    let _ = Duration::from_secs(60 * x);
    let _ = std::time::Duration::from_secs(x * 60);
    let _ = std::time::Duration::from_secs(60 * x);
    let _ = Duration::from_secs(x * 3600);
    let _ = std::time::Duration::from_secs(3600 * x);
    let _ = Duration::from_millis(x * 60);
    let _ = Duration::from_secs(x * 61);
    let _ = Duration::from_secs(x + 60);
    let _ = other::from_secs(x * 60);
    println!(\"{}\", x * 60);
    a + b
}
";
        let Some(found) = rust_matches(code) else { return };
        let lines: Vec<usize> = found.iter().filter(|(r, _)| r == "rust-duration-from-mins-hours").map(|(_, l)| *l).collect();
        assert_eq!(lines, vec![5, 6, 7, 8, 9, 10], "{found:?}");
    }

    #[test]
    fn match_reads_confidence_from_metadata() {
        let v: Value = serde_json::from_str(
            r#"{"file":"a.py","range":{"start":{"line":0,"column":0}},"ruleId":"python-asyncio-gather","message":"m","metadata":{"gate":"requires-python","min":"3.11","confidence":"low"}}"#,
        )
        .unwrap();
        let m = parse_match(&v).unwrap();
        assert_eq!(m.confidence, Confidence::Low);
        let f = to_finding("python", m, Some(">=3.12"), false, "");
        assert_eq!((f.kind, f.confidence), (Kind::Modern, Confidence::Low));
        assert!(!f.stops_gate());
    }
}
