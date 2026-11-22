//! Findings, per-stack results, the JSON report and the stderr summary.

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Available under the current pins.
    Modern,
    /// Needs a newer toolchain or pin; `upgrade` says which.
    NeedsNewer,
    /// The target (browsers, PowerShell host) may lack what the code uses.
    Baseline,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Modern => "modern",
            Kind::NeedsNewer => "needs-newer",
            Kind::Baseline => "baseline",
        }
    }
}

/// How sure the rule is that the old form should change. `--gate` stops
/// only on `High` `modern` findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// A rule engine's own rule, or a catalogue rule that matches the old form exactly.
    High,
    /// A catalogue heuristic: the old form is often right where it is.
    Low,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Low => "low",
        }
    }

    /// A rule's `metadata.confidence`; anything but `high` (or nothing) is `Low`.
    pub fn parse(text: &str) -> Confidence {
        if text.trim().eq_ignore_ascii_case("high") { Confidence::High } else { Confidence::Low }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upgrade {
    pub pin: String,
    pub from: String,
    pub to: String,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub stack: &'static str,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub rule: String,
    pub message: String,
    pub kind: Kind,
    pub confidence: Confidence,
    /// "tool" or "ast-grep".
    pub source: &'static str,
    pub upgrade: Option<Upgrade>,
}

impl Finding {
    pub fn to_json(&self) -> Value {
        json!({
            "stack": self.stack,
            "file": self.file,
            "line": self.line,
            "column": self.column,
            "rule": self.rule,
            "message": self.message,
            "kind": self.kind.as_str(),
            "confidence": self.confidence.as_str(),
            "source": self.source,
            "upgrade": self.upgrade.as_ref().map(|u| json!({"pin": u.pin, "from": u.from, "to": u.to, "note": u.note})),
        })
    }

    /// Whether `--gate` stops on this finding.
    pub fn stops_gate(&self) -> bool {
        self.kind == Kind::Modern && self.confidence == Confidence::High
    }
}

/// The exit code: `1` when a finding counts (with `gate`, only high-confidence
/// `modern` findings count; the rest are warnings), else `2` when a stack
/// failed, else `0`.
pub fn exit_code(findings: &[Finding], failed: bool, gate: bool) -> i32 {
    let counted = if gate { findings.iter().any(Finding::stops_gate) } else { !findings.is_empty() };
    if counted {
        1
    } else if failed {
        2
    } else {
        0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Skipped,
    Error,
}

#[derive(Debug, Clone)]
pub struct StackReport {
    pub stack: &'static str,
    pub files: Vec<String>,
    pub pins: Map<String, Value>,
    pub newest: Map<String, Value>,
    pub tool: String,
    pub status: Status,
    pub messages: Vec<String>,
    pub duration_ms: u64,
    pub findings: Vec<Finding>,
}

impl StackReport {
    pub fn new(stack: &'static str, files: Vec<String>) -> StackReport {
        StackReport {
            stack,
            files,
            pins: Map::new(),
            newest: Map::new(),
            tool: String::new(),
            status: Status::Ok,
            messages: Vec::new(),
            duration_ms: 0,
            findings: Vec::new(),
        }
    }
    pub fn note(&mut self, text: impl Into<String>) {
        let t = text.into();
        if !t.is_empty() && !self.messages.contains(&t) {
            self.messages.push(t);
        }
    }
    pub fn skip(&mut self, why: impl Into<String>) {
        self.status = Status::Skipped;
        self.note(why);
    }
    pub fn fail(&mut self, why: impl Into<String>) {
        self.status = Status::Error;
        self.note(why);
    }
    pub fn pin(&mut self, key: &str, value: impl Into<Value>) {
        self.pins.insert(key.to_string(), value.into());
    }
    pub fn newest(&mut self, key: &str, value: impl Into<Value>) {
        self.newest.insert(key.to_string(), value.into());
    }
    pub fn to_json(&self) -> Value {
        json!({
            "stack": self.stack,
            "files": self.files,
            "pins": self.pins,
            "newest": self.newest,
            "tool": self.tool,
            "status": match self.status { Status::Ok => "ok", Status::Skipped => "skipped", Status::Error => "error" },
            "message": self.messages.join(" "),
            "duration_ms": self.duration_ms,
        })
    }
}

/// Sorted, de-duplicated findings (same place, rule and kind once).
pub fn tidy(findings: &mut Vec<Finding>) {
    findings.sort_by(|a, b| (a.file.as_str(), a.line, a.column, a.rule.as_str(), a.stack).cmp(&(b.file.as_str(), b.line, b.column, b.rule.as_str(), b.stack)));
    findings.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.column == b.column && a.rule == b.rule && a.kind == b.kind);
}

pub fn document(version: &str, repo: &str, change: &Value, stacks: &[StackReport], findings: &[Finding]) -> Value {
    json!({
        "tool": "modern-tara",
        "version": version,
        "repo": repo,
        "change": change,
        "stacks": stacks.iter().map(StackReport::to_json).collect::<Vec<_>>(),
        "findings": findings.iter().map(Finding::to_json).collect::<Vec<_>>(),
    })
}

/// `2 modern high, 1 modern low, 1 needs-newer high`, or `no findings`.
fn counts<'a>(findings: impl Iterator<Item = &'a Finding>) -> String {
    let mut by: BTreeMap<(Kind, Confidence), usize> = BTreeMap::new();
    for f in findings {
        *by.entry((f.kind, f.confidence)).or_default() += 1;
    }
    if by.is_empty() {
        return "no findings".to_string();
    }
    by.iter().map(|((k, c), n)| format!("{n} {} {}", k.as_str(), c.as_str())).collect::<Vec<_>>().join(", ")
}

/// A few lines for a human: per stack, status, counts per kind and
/// confidence, time. `gate` is the `--gate` exit code: a final line says
/// whether the gate passed (none for a tool error).
pub fn summary(stacks: &[StackReport], findings: &[Finding], total_ms: u64, gate: Option<i32>) -> String {
    let mut out = String::new();
    for s in stacks {
        let counts = counts(findings.iter().filter(|f| f.stack == s.stack));
        let status = match s.status {
            Status::Ok => "ok",
            Status::Skipped => "skipped",
            Status::Error => "ERROR",
        };
        let mut line = format!("{:<10} {:<7} {:>3} files  {:<44} {:>6.1}s", s.stack, status, s.files.len(), counts, ms_to_s(s.duration_ms));
        if s.status != Status::Ok && !s.messages.is_empty() {
            let msg: String = s.messages.join(" ").chars().take(160).collect();
            line.push_str("  ");
            line.push_str(&msg);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    if findings.is_empty() {
        let _ = writeln!(out, "modern-tara: 0 findings in {:.1}s", ms_to_s(total_ms));
    } else {
        let _ = writeln!(out, "modern-tara: {} findings ({}) in {:.1}s", findings.len(), counts(findings.iter()), ms_to_s(total_ms));
    }
    match gate {
        Some(0) => out.push_str("modern-tara: gate passed\n"),
        Some(1) => {
            let n = findings.iter().filter(|f| f.stops_gate()).count();
            let _ = writeln!(out, "modern-tara: gate stopped ({n} high-confidence modern findings)");
        }
        _ => {}
    }
    out
}

#[expect(clippy::cast_precision_loss, reason = "durations are far below 2^52 ms")]
fn ms_to_s(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(file: &str, line: u32, kind: Kind) -> Finding {
        Finding {
            stack: "python",
            file: file.into(),
            line,
            column: 1,
            rule: "UP006".into(),
            message: "m".into(),
            kind,
            source: "tool",
            confidence: crate::report::Confidence::High,
            upgrade: (kind == Kind::NeedsNewer).then(|| Upgrade { pin: "requires-python".into(), from: ">=3.8".into(), to: "3.9".into(), note: String::new() }),
        }
    }

    #[test]
    fn json_shape() {
        let mut s = StackReport::new("python", vec!["a.py".into()]);
        s.pin("requires-python", ">=3.8");
        s.newest("python", "3.15");
        s.tool = "ruff 0.17.6".into();
        let f = vec![finding("a.py", 3, Kind::Modern), finding("a.py", 4, Kind::NeedsNewer)];
        let doc = document("0.1.0", "C:/r", &json!({"mode": "range"}), &[s], &f);
        assert_eq!(doc["tool"], "modern-tara");
        let stack = &doc["stacks"][0];
        for key in ["stack", "files", "pins", "newest", "tool", "status", "message", "duration_ms"] {
            assert!(stack.get(key).is_some(), "stack.{key}");
        }
        assert_eq!(stack["status"], "ok");
        let f0 = &doc["findings"][0];
        for key in ["stack", "file", "line", "column", "rule", "message", "kind", "confidence", "source", "upgrade"] {
            assert!(f0.get(key).is_some(), "finding.{key}");
        }
        assert!(f0["upgrade"].is_null());
        assert_eq!(f0["confidence"], "high");
        assert_eq!(doc["findings"][1]["kind"], "needs-newer");
        assert_eq!(doc["findings"][1]["upgrade"]["pin"], "requires-python");
    }

    fn low(mut f: Finding) -> Finding {
        f.confidence = Confidence::Low;
        f
    }

    #[test]
    fn gate_counts_only_high_confidence_modern() {
        let high = vec![finding("a.py", 1, Kind::Modern), low(finding("a.py", 2, Kind::Modern))];
        let warnings = vec![low(finding("a.py", 2, Kind::Modern)), finding("a.py", 3, Kind::NeedsNewer), finding("a.py", 4, Kind::Baseline)];
        assert_eq!(exit_code(&high, false, true), 1);
        assert_eq!(exit_code(&warnings, false, true), 0);
        assert_eq!(exit_code(&warnings, false, false), 1, "without --gate every finding counts");
        assert_eq!(exit_code(&warnings, true, true), 2, "a failed stack is a tool error");
        assert_eq!(exit_code(&high, true, true), 1);
        assert_eq!(exit_code(&[], false, true), 0);
        assert_eq!(exit_code(&[], true, false), 2);
    }

    #[test]
    fn summary_counts_kind_and_confidence_and_says_the_gate() {
        let s = StackReport::new("python", vec!["a.py".into()]);
        let f = vec![
            finding("a.py", 1, Kind::Modern),
            finding("a.py", 2, Kind::Modern),
            low(finding("a.py", 3, Kind::Modern)),
            finding("a.py", 4, Kind::NeedsNewer),
        ];
        let text = summary(std::slice::from_ref(&s), &f, 1500, Some(1));
        assert!(text.contains("2 modern high, 1 modern low, 1 needs-newer high"), "{text}");
        assert!(text.contains("modern-tara: 4 findings (2 modern high, 1 modern low, 1 needs-newer high) in 1.5s"), "{text}");
        assert!(text.ends_with("modern-tara: gate stopped (2 high-confidence modern findings)\n"), "{text}");
        let passed = summary(std::slice::from_ref(&s), &f[2..], 0, Some(0));
        assert!(passed.ends_with("modern-tara: gate passed\n"), "{passed}");
        assert!(!summary(&[s], &[], 0, None).contains("modern-tara: gate"));
        assert_eq!(Confidence::parse("HIGH"), Confidence::High);
        assert_eq!(Confidence::parse(""), Confidence::Low);
    }

    #[test]
    fn tidy_dedups() {
        let mut v = vec![finding("b.py", 1, Kind::Modern), finding("a.py", 2, Kind::Modern), finding("a.py", 2, Kind::Modern)];
        tidy(&mut v);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].file, "a.py");
    }
}
