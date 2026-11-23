//! PowerShell: `PSScriptAnalyzer` through `ps/run-pssa.ps1`, with the host each
//! script runs under (pwsh 7 or Windows PowerShell 5.1) deciding the syntax
//! target of `PSUseCompatibleSyntax`.

use super::Ctx;
use crate::change::{self, Change};
use crate::pins::NEWEST_PWSH;
use crate::proc::Cmd;
use crate::report::{Finding, Kind, StackReport};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;

/// Only rules that move code to a newer construct (WMI → CIM cmdlets) or check
/// the host's syntax. Left out as style, not age: `PSAvoidUsingWriteHost`
/// (output streams), `PSAvoidUsingCmdletAliases` (alias vs. full name) and
/// `PSAvoidUsingPositionalParameters` (parameter names).
pub const RULES: [&str; 2] = ["PSAvoidUsingWMICmdlet", "PSUseCompatibleSyntax"];

/// Files where a launcher may name a script.
const LAUNCHER_EXT: [&str; 13] = ["cmd", "bat", "ps1", "psm1", "psd1", "xml", "json", "yml", "yaml", "vbs", "reg", "txt", "sh"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Host {
    Pwsh7,
    Desktop51,
    Unknown,
}

impl Host {
    fn label(self) -> &'static str {
        match self {
            Host::Pwsh7 => "pwsh 7",
            Host::Desktop51 => "Windows PowerShell 5.1",
            Host::Unknown => "unknown",
        }
    }
    fn targets(self) -> &'static str {
        match self {
            Host::Pwsh7 => "'7.0'",
            Host::Desktop51 => "'5.1'",
            Host::Unknown => "'5.1','7.0'",
        }
    }
}

/// From the script's own `#Requires` line.
pub fn host_from_requires(script: &str) -> Option<Host> {
    for line in script.lines() {
        let l = line.trim().to_ascii_lowercase();
        let Some(rest) = l.strip_prefix("#requires") else { continue };
        if rest.contains("-psedition core") {
            return Some(Host::Pwsh7);
        }
        if rest.contains("-psedition desktop") {
            return Some(Host::Desktop51);
        }
        if let Some(i) = rest.find("-version") {
            let v = rest[i + 8..].trim_start();
            let major: String = v.chars().take_while(char::is_ascii_digit).collect();
            if major.parse::<u32>().is_ok_and(|m| m >= 6) {
                return Some(Host::Pwsh7);
            }
        }
    }
    None
}

/// From launchers that name the script within three lines of `pwsh` or
/// `powershell.exe`. If both appear, 5.1 (the stricter host) wins.
pub fn host_from_launchers(script_name: &str, launchers: &[(String, String)]) -> Option<(Host, String)> {
    let needle = script_name.to_lowercase();
    let mut found: Option<(Host, String)> = None;
    for (path, text) in launchers {
        let lines: Vec<String> = text.lines().map(str::to_lowercase).collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains(&needle) {
                continue;
            }
            let window = lines[i.saturating_sub(3)..(i + 4).min(lines.len())].join("\n");
            let desktop = window.contains("powershell.exe") || window.contains("powershell -") || window.contains("powershell \"");
            let core = window.contains("pwsh");
            let host = if desktop {
                Host::Desktop51
            } else if core {
                Host::Pwsh7
            } else {
                continue;
            };
            if found.as_ref().is_none_or(|(h, _)| host == Host::Desktop51 && *h != Host::Desktop51) {
                found = Some((host, path.clone()));
            }
        }
    }
    found
}

#[expect(clippy::too_many_lines, reason = "one linear pipeline: pins, copy, run, parse")]
pub fn run(ctx: &Ctx, files: &[String]) -> StackReport {
    let mut report = StackReport::new("powershell", files.to_vec());
    report.newest("pwsh", NEWEST_PWSH);
    let Some(pwsh) = ctx.tools.pwsh.clone() else {
        report.skip("pwsh not found: install PowerShell 7, then `Install-PSResource PSScriptAnalyzer -Scope CurrentUser`.");
        return report;
    };
    let installed = super::version_line(&pwsh, &["--version"]).unwrap_or_default();
    let installed = installed.trim_start_matches("PowerShell ").to_string();
    report.newest("installed_pwsh", installed.clone());
    let change = ctx.change;
    let launchers = launcher_texts(change, files);
    let mut groups: BTreeMap<Host, Vec<String>> = BTreeMap::new();
    let mut pins_json = serde_json::Map::new();
    for f in files {
        let text = change.read_text(f).unwrap_or_default();
        let (host, why) = match host_from_requires(&text) {
            Some(h) => (h, "#Requires".to_string()),
            None => match host_from_launchers(change::file_name(f), &launchers) {
                Some((h, by)) => (h, format!("launched in {by}")),
                None => (Host::Unknown, "no #Requires and no launcher found".into()),
            },
        };
        pins_json.insert(f.clone(), json!({"host": host.label(), "why": why}));
        groups.entry(host).or_default().push(f.clone());
    }
    report.pin("hosts", Value::Object(pins_json));
    if groups.contains_key(&Host::Desktop51) {
        report.note(format!(
            "needs-newer: some scripts run under Windows PowerShell 5.1; PowerShell {NEWEST_PWSH} is the newest stable (installed {installed})."
        ));
    }

    let dir = match ctx.temp.sub("powershell") {
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
    let mut job_groups = Vec::new();
    for (host, members) in &groups {
        let settings = dir.join(format!("modern-tara-{}.psd1", job_groups.len()));
        let rules = RULES.iter().map(|r| format!("'{r}'")).collect::<Vec<_>>().join(",");
        let psd1 = format!(
            "@{{\n  IncludeRules = @({rules})\n  Rules = @{{ PSUseCompatibleSyntax = @{{ Enable = $true; TargetVersions = @({}) }} }}\n}}\n",
            host.targets()
        );
        if let Err(e) = fs::write(&settings, psd1) {
            report.fail(e.to_string());
            return report;
        }
        let abs: Vec<String> = members.iter().filter_map(|m| change::safe_join(&dir, m)).map(|p| p.to_string_lossy().into_owned()).collect();
        job_groups.push(json!({"settings": settings.to_string_lossy(), "files": abs, "host": host.label()}));
    }
    let job_file = ctx.temp.path().join("pssa-job.json");
    let out_file = ctx.temp.path().join("pssa-out.json");
    if let Err(e) = fs::write(&job_file, json!({"groups": job_groups}).to_string()) {
        report.fail(e.to_string());
        return report;
    }
    let out = Cmd::new(&pwsh)
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(ctx.home.join("ps").join("run-pssa.ps1"))
        .arg("-Job")
        .arg(&job_file)
        .arg("-Out")
        .arg(&out_file)
        .cwd(&dir)
        .timeout(ctx.timeout_or(180))
        .run();
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            report.fail(e);
            return report;
        }
    };
    if out.timed_out {
        report.fail(format!("PSScriptAnalyzer timed out after {}s (raise --timeout)", out.ms / 1000));
        return report;
    }
    let Ok(text) = fs::read_to_string(&out_file) else {
        report.fail(format!("PSScriptAnalyzer runner failed: {}", out.tail(3)));
        return report;
    };
    let v: Value = match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
        Ok(v) => v,
        Err(e) => {
            report.fail(format!("PSScriptAnalyzer output: {e}"));
            return report;
        }
    };
    if v.get("missing").and_then(Value::as_bool) == Some(true) {
        report.skip("PSScriptAnalyzer not installed: `Install-PSResource PSScriptAnalyzer -Scope CurrentUser`.");
        return report;
    }
    report.tool = format!("PSScriptAnalyzer {} on pwsh {installed}", v.get("pssa").and_then(Value::as_str).unwrap_or("?"));
    report.ran(RULES);
    for e in v.get("errors").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        report.note(e.replace(&*dir.to_string_lossy(), ""));
    }
    for p in v.get("failed").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        if let Some(file) = super::rel_to(&dir, p) {
            report.unscanned(file, "PSScriptAnalyzer failed on it");
        }
    }
    let results: Vec<Value> = match v.get("results") {
        Some(Value::Array(a)) => a.clone(),
        Some(o @ Value::Object(_)) => vec![o.clone()],
        _ => Vec::new(),
    };
    let host_of: BTreeMap<&String, Host> = groups.iter().flat_map(|(h, fs)| fs.iter().map(move |f| (f, *h))).collect();
    for r in results {
        let Some(file) = r.get("ScriptPath").and_then(Value::as_str).and_then(|p| super::rel_to(&dir, p)) else { continue };
        let rule = r.get("RuleName").and_then(Value::as_str).unwrap_or("").to_string();
        let num = |k: &str| r.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
        let mut message = r.get("Message").and_then(Value::as_str).unwrap_or("").to_string();
        // A syntax error comes back as a record: the file is not analysed, never a
        // finding. A module or type that is not installed here is only a note.
        if r.get("Severity").and_then(Value::as_str) == Some("ParseError") {
            if matches!(rule.as_str(), "ModuleNotFoundDuringParse" | "TypeNotFound") {
                report.note(format!("{file}: {rule}: {message}"));
            } else {
                report.unscanned(file, format!("not parsed: {rule}: {message}"));
            }
            continue;
        }
        let compat = rule == "PSUseCompatibleSyntax";
        if compat && host_of.get(&file) == Some(&Host::Unknown) {
            message = format!("If run by Windows PowerShell 5.1: {message}");
        }
        report.findings.push(Finding {
            stack: "powershell",
            file,
            line: num("Line"),
            column: num("Column"),
            rule,
            message,
            kind: if compat { Kind::Baseline } else { Kind::Modern },
            source: "tool",
            confidence: crate::report::Confidence::High,
            upgrade: None,
        });
    }
    report
}

/// Texts of files that may launch scripts and mention one of `files`.
fn launcher_texts(change: &Change, files: &[String]) -> Vec<(String, String)> {
    let names: Vec<String> = files.iter().map(|f| change::file_name(f).to_lowercase()).collect();
    let candidates: Vec<String> = change
        .listing()
        .unwrap_or_default()
        .iter()
        .filter(|p| LAUNCHER_EXT.contains(&change::extension(p).as_str()) && !files.contains(p))
        .take(4000)
        .cloned()
        .collect();
    let Ok(contents) = change.read(&candidates) else { return Vec::new() };
    let mut out: Vec<(String, String)> = contents
        .into_iter()
        .filter(|(_, b)| b.len() <= 2 * 1024 * 1024)
        .map(|(p, b)| (p, decode(&b)))
        .filter(|(_, t)| {
            let lower = t.to_lowercase();
            names.iter().any(|n| lower.contains(n))
        })
        .collect();
    out.sort();
    out
}

/// UTF-8, or UTF-16 LE with BOM (scheduled task XML exports).
fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_lines() {
        assert_eq!(host_from_requires("#Requires -Version 7.2\n"), Some(Host::Pwsh7));
        assert_eq!(host_from_requires("#requires -PSEdition Core"), Some(Host::Pwsh7));
        assert_eq!(host_from_requires("#Requires -PSEdition Desktop"), Some(Host::Desktop51));
        assert_eq!(host_from_requires("#Requires -Version 5.1"), None);
        assert_eq!(host_from_requires("Write-Output 1"), None);
    }

    #[test]
    fn launchers_decide_host() {
        let task = ("task.xml".to_string(), "<Exec>\n<Command>powershell.exe</Command>\n<Arguments>-File C:\\x\\Job.ps1</Arguments>\n</Exec>".to_string());
        let cmd = ("run.cmd".to_string(), "pwsh -NoProfile -File other.ps1\n".to_string());
        assert_eq!(host_from_launchers("job.ps1", &[task.clone(), cmd.clone()]).map(|h| h.0), Some(Host::Desktop51));
        assert_eq!(host_from_launchers("other.ps1", &[task, cmd]).map(|h| h.0), Some(Host::Pwsh7));
        assert_eq!(host_from_launchers("none.ps1", &[]), None);
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("pwsh a.ps1".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(decode(&utf16), "pwsh a.ps1");
    }

    #[test]
    fn style_only_rules_are_not_run() {
        for dropped in ["PSAvoidUsingPositionalParameters", "PSAvoidUsingCmdletAliases", "PSAvoidUsingWriteHost"] {
            assert!(!RULES.contains(&dropped), "{dropped}");
        }
    }
}
