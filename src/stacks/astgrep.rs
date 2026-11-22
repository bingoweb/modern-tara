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

/// Runs every rule over `files` (relative to `copy`). `Err` only when
/// ast-grep exists but failed; a missing ast-grep gives `Ok(None)`.
pub fn scan(ctx: &Ctx, copy: &Path, files: &[String]) -> Result<Option<Vec<Match>>, String> {
    let Some(exe) = &ctx.tools.ast_grep else { return Ok(None) };
    if files.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let config = ctx.home.join("sgconfig.yml");
    if !config.is_file() {
        return Err(format!("{} is missing", config.display()));
    }
    let mut matches = Vec::new();
    // Chunks keep the command line under the Windows limit.
    for chunk in files.chunks(200) {
        let out = Cmd::new(exe)
            .arg("scan")
            .arg("--config")
            .arg(&config)
            .args(["--json=stream", "--include-metadata", "--color", "never"])
            .args(chunk)
            .cwd(copy)
            .timeout(ctx.timeout_or(60))
            .run()?;
        if out.timed_out {
            return Err("ast-grep timed out".into());
        }
        // Exit 1 only means an error-severity match; the rules use info/hint.
        if !matches!(out.code, Some(0 | 1)) {
            return Err(format!("ast-grep failed: {}", out.tail(2)));
        }
        for line in out.stdout_text().lines().filter(|l| l.starts_with('{')) {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if let Some(m) = parse_match(&v) {
                matches.push(m);
            }
        }
    }
    Ok(Some(matches))
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
