//! Version pins read from project files. Pure text functions, so each one is
//! unit-tested without a repository.

use std::collections::BTreeMap;

/// Newest stable releases (STS included) as of this build. C# and .NET come
/// from this table because only SDK 10 is installed.
pub const NEWEST_DOTNET: &str = "11";
pub const NEWEST_CSHARP: &str = "15";
pub const NEWEST_RUST: &str = "1.100";
pub const NEWEST_EDITION: &str = "2024";
pub const NEWEST_PYTHON: (u32, u32) = (3, 15);
pub const NEWEST_NODE: &str = "26";
pub const NEWEST_ECMASCRIPT: &str = "2026";
pub const NEWEST_PWSH: &str = "7.7";
pub const NEWEST_GO: &str = "1.27";

// ---------- MSBuild ----------

/// Properties that matter, from one project or props file. The first
/// unconditioned definition wins; a conditioned one only fills a gap.
pub fn msbuild_props(text: &str) -> BTreeMap<String, String> {
    const NAMES: [&str; 5] = ["TargetFramework", "TargetFrameworks", "LangVersion", "AnalysisLevel", "EnableNETAnalyzers"];
    let mut out = BTreeMap::new();
    let mut conditioned = BTreeMap::new();
    for name in NAMES {
        let mut rest = text;
        while let Some(at) = rest.find(&format!("<{name}")) {
            let after = &rest[at + name.len() + 1..];
            let Some(close) = after.find('>') else { break };
            let attrs = &after[..close];
            // `<TargetFrameworkVersion>` and friends start with the same text.
            if attrs.starts_with(|c: char| c.is_alphanumeric()) || attrs.ends_with('/') {
                rest = &after[close..];
                continue;
            }
            let body = &after[close + 1..];
            let Some(end) = body.find(&format!("</{name}>")) else { break };
            let value = body[..end].trim().to_string();
            if attrs.contains("Condition") {
                conditioned.entry(name.to_string()).or_insert(value);
            } else {
                out.entry(name.to_string()).or_insert(value);
            }
            rest = &body[end..];
        }
    }
    for (k, v) in conditioned {
        out.entry(k).or_insert(v);
    }
    out
}

/// Includes of `<ProjectReference Include="...">`, as written.
pub fn project_references(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("<ProjectReference") {
        let after = &rest[at..];
        let end = after.find('>').unwrap_or(after.len());
        let tag = &after[..end];
        if let Some(i) = tag.find("Include=") {
            let quoted = &tag[i + 8..];
            if let Some(q) = quoted.chars().next().filter(|c| *c == '"' || *c == '\'')
                && let Some(close) = quoted[1..].find(q)
            {
                out.push(quoted[1..=close].to_string());
            }
        }
        rest = &after[end.min(after.len())..];
        if end == 0 {
            break;
        }
    }
    out
}

/// Whether a Directory.Build.props pulls in the one above it.
pub fn imports_parent_props(text: &str) -> bool {
    text.contains("GetPathOfFileAbove") && text.contains("Directory.Build.props")
}

/// Effective properties: farthest props first, nearest last, project last.
pub fn merge_props(chain_far_to_near: &[BTreeMap<String, String>]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for layer in chain_far_to_near {
        for (k, v) in layer {
            out.insert(k.clone(), v.clone());
        }
    }
    if out.contains_key("TargetFrameworks") && out.contains_key("TargetFramework") {
        // The plural wins in MSBuild's outer build.
        out.remove("TargetFramework");
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tfm {
    Framework(u32, u32),
    Standard(u32, u32),
    /// .NET Core 1-3 and .NET 5+.
    Core(u32, u32),
}

pub fn parse_tfm(tfm: &str) -> Option<Tfm> {
    let t = tfm.trim().to_ascii_lowercase();
    let base = t.split('-').next()?;
    if let Some(v) = base.strip_prefix("netstandard") {
        let (a, b) = major_minor(v)?;
        return Some(Tfm::Standard(a, b));
    }
    if let Some(v) = base.strip_prefix("netcoreapp") {
        let (a, b) = major_minor(v)?;
        return Some(Tfm::Core(a, b));
    }
    let v = base.strip_prefix("net")?;
    if v.contains('.') {
        let (a, b) = major_minor(v)?;
        return Some(Tfm::Core(a, b));
    }
    // net48, net472: .NET Framework.
    let digits: Vec<u32> = v.chars().map(|c| c.to_digit(10)).collect::<Option<_>>()?;
    match digits.as_slice() {
        [a, b, ..] => Some(Tfm::Framework(*a, *b)),
        _ => None,
    }
}

/// The oldest framework of `TargetFramework(s)`, the one that gates APIs.
pub fn lowest_tfm(props: &BTreeMap<String, String>) -> Option<(String, Tfm)> {
    let list = props.get("TargetFrameworks").or_else(|| props.get("TargetFramework"))?;
    list.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.contains('$'))
        .filter_map(|s| parse_tfm(s).map(|t| (s.to_string(), t)))
        .min_by_key(|(_, t)| *t)
}

/// Whether `tfm` has an API that shipped in .NET `major.minor`.
pub fn tfm_at_least(tfm: Tfm, major: u32, minor: u32) -> bool {
    match tfm {
        Tfm::Core(a, b) => (a, b) >= (major, minor),
        Tfm::Framework(..) | Tfm::Standard(..) => false,
    }
}

/// `global.json` → `sdk.version`, `sdk.rollForward`.
pub fn global_json(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&strip_json_comments(text)) {
        for key in ["version", "rollForward"] {
            if let Some(s) = v.pointer(&format!("/sdk/{key}")).and_then(|x| x.as_str()) {
                out.insert(format!("sdk.{key}"), s.to_string());
            }
        }
    }
    out
}

// ---------- Cargo ----------

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CargoPins {
    pub name: Option<String>,
    pub edition: Option<String>,
    pub rust_version: Option<String>,
    pub is_workspace: bool,
    pub members: Vec<String>,
    pub exclude: Vec<String>,
    pub has_package: bool,
}

/// `[package]` and `[workspace]` of one manifest; `workspace = true` keys are
/// resolved against `workspace_root` (the root manifest's text).
pub fn cargo_pins(text: &str, workspace_root: Option<&str>) -> Result<CargoPins, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("Cargo.toml: {e}"))?;
    let root: Option<toml::Table> = workspace_root.and_then(|t| t.parse().ok());
    let mut pins = CargoPins::default();
    if let Some(pkg) = table.get("package").and_then(|p| p.as_table()) {
        pins.has_package = true;
        pins.name = pkg.get("name").and_then(|v| v.as_str()).map(String::from);
        let inherited = |key: &str| -> Option<String> {
            let v = pkg.get(key)?;
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
            let wants = v.as_table()?.get("workspace")?.as_bool()?;
            if !wants {
                return None;
            }
            let ws = root.as_ref().or(Some(&table))?;
            ws.get("workspace")?.get("package")?.get(key)?.as_str().map(String::from)
        };
        pins.edition = inherited("edition");
        pins.rust_version = inherited("rust-version");
    }
    if let Some(ws) = table.get("workspace").and_then(|w| w.as_table()) {
        pins.is_workspace = true;
        let strings = |key: &str| -> Vec<String> {
            ws.get(key).and_then(|v| v.as_array()).map_or_default(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        };
        pins.members = strings("members");
        pins.exclude = strings("exclude");
        if !pins.has_package {
            let wp = ws.get("package");
            pins.edition = wp.and_then(|p| p.get("edition")).and_then(|v| v.as_str()).map(String::from);
            pins.rust_version = wp.and_then(|p| p.get("rust-version")).and_then(|v| v.as_str()).map(String::from);
        }
    }
    Ok(pins)
}

/// Whether `crate_rel` (relative to the workspace root) is one of its members.
pub fn is_member(pins: &CargoPins, crate_rel: &str) -> bool {
    if crate_rel.is_empty() {
        return true;
    }
    let matches = |pats: &[String]| pats.iter().any(|m| crate::change::glob_match(m.trim_end_matches('/'), crate_rel));
    matches(&pins.members) && !matches(&pins.exclude)
}

/// `1.85`, `1.85.0` into a comparable pair.
pub fn rust_version(v: &str) -> Option<(u32, u32)> {
    major_minor(v.trim())
}

// ---------- Python ----------

/// Lowest Python a `requires-python` specifier allows, e.g. `>=3.12,<4`.
pub fn requires_python_min(spec: &str) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    for clause in spec.split(',') {
        let c = clause.trim();
        let version = if let Some(v) = c.strip_prefix(">=").or_else(|| c.strip_prefix("~=")).or_else(|| c.strip_prefix("===")) {
            v
        } else if let Some(v) = c.strip_prefix("==").or_else(|| c.strip_prefix('>')) {
            v
        } else if c.starts_with(|ch: char| ch.is_ascii_digit()) {
            c
        } else {
            continue;
        };
        if let Some(v) = major_minor(version.trim().trim_end_matches(".*")) {
            best = Some(best.map_or(v, |b| b.max(v)));
        }
    }
    best
}

/// `.python-version`: the first `X.Y` on a non-comment line.
pub fn python_version_file(text: &str) -> Option<(u32, u32)> {
    text.lines().map(|l| l.trim().trim_start_matches('\u{feff}')).filter(|l| !l.is_empty() && !l.starts_with('#')).find_map(|l| {
        let start = l.find(|c: char| c.is_ascii_digit())?;
        major_minor(&l[start..])
    })
}

// ---------- Node ----------

/// Lowest Node version an `engines.node` range admits: `>=18 <23`, `^20.11`,
/// `18.x || 20.x`, `14 - 18`. `None` when unreadable.
pub fn node_min(range: &str) -> Option<(u32, u32, u32)> {
    let mut lowest: Option<(u32, u32, u32)> = None;
    for alt in range.split("||") {
        let alt = alt.trim();
        if alt.is_empty() {
            continue;
        }
        let bound = if let Some((lo, _)) = alt.split_once(" - ") {
            semver_floor(lo.trim())?
        } else {
            let mut floor = (0, 0, 0);
            let tokens = alt.replace(">= ", ">=").replace("> ", ">").replace("<= ", "<=").replace("< ", "<").replace("^ ", "^").replace("~ ", "~");
            for tok in tokens.split_whitespace() {
                if tok.starts_with('<') {
                    continue;
                }
                let v = tok.trim_start_matches(['>', '=', '^', '~', 'v']);
                if v == "*" || v.eq_ignore_ascii_case("x") || v.is_empty() {
                    continue;
                }
                floor = floor.max(semver_floor(v)?);
            }
            floor
        };
        lowest = Some(lowest.map_or(bound, |l| l.min(bound)));
    }
    lowest
}

fn semver_floor(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.trim_start_matches('v');
    let mut parts = v.split('.').map(|p| if p == "x" || p == "X" || p == "*" { Some(0) } else { p.parse::<u32>().ok() });
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0)).unwrap_or(0);
    Some((major, minor, patch))
}

/// `available` option of stylelint-plugin-use-baseline from browserslist
/// queries: `baseline widely available`, `baseline newly available`,
/// `baseline 2023`; anything else is "widely".
pub fn baseline_available(queries: &[String]) -> serde_json::Value {
    for q in queries {
        let q = q.trim().to_ascii_lowercase();
        if let Some(rest) = q.strip_prefix("baseline ") {
            if rest.starts_with("newly") {
                return "newly".into();
            }
            if rest.starts_with("widely") {
                return "widely".into();
            }
            if let Ok(year) = rest.split_whitespace().next().unwrap_or("").parse::<u32>() {
                return year.into();
            }
        }
    }
    "widely".into()
}

/// Browserslist queries from a package.json value (string, array, or
/// per-environment object: `production` first).
pub fn browserslist_from_json(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        serde_json::Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        serde_json::Value::Object(o) => o.get("production").or_else(|| o.values().next()).map_or_default(browserslist_from_json),
        _ => Vec::new(),
    }
}

/// `.browserslistrc`: default section lines.
pub fn browserslistrc(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.split('#').next().unwrap_or("").trim();
        if l.starts_with('[') {
            if !out.is_empty() {
                break;
            }
            continue;
        }
        out.extend(l.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()));
    }
    out
}

// ---------- Go ----------

/// A Go version as go.mod and `go env GOVERSION` write it. The order is the
/// go command's: `1.21` < `1.21beta1` < `1.21rc1` < `1.21.0` < `1.21.1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GoVersion {
    pub major: u32,
    pub minor: u32,
    /// 0: language version only (`1.21`), 1: beta, 2: rc, 3: release (`1.21.0`).
    stage: u8,
    /// The beta/rc number, or the patch.
    n: u32,
}

impl GoVersion {
    pub fn lang(major: u32, minor: u32) -> GoVersion {
        GoVersion { major, minor, stage: 0, n: 0 }
    }
    /// `1.24`, for `upgrade.to` and step copies.
    pub fn minor_text(self) -> String {
        format!("{}.{}", self.major, self.minor)
    }
}

/// `1.26.0`, `1.22`, `1.26rc1`, `go1.27.4` (the `go` prefix is optional);
/// `None` for anything else, like the go.mod parser.
pub fn go_version(text: &str) -> Option<GoVersion> {
    let v = text.trim();
    let v = v.trim_prefix("go");
    let number = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))).then(|| s.parse().ok()).flatten();
    let (major, rest) = v.split_once('.')?;
    let major: u32 = number(major).filter(|m| *m >= 1)?;
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let minor = number(&rest[..digits])?;
    let mut rest = &rest[digits..];
    let mut out = GoVersion::lang(major, minor);
    if let Some(p) = rest.strip_prefix('.') {
        let end = p.find(|c: char| !c.is_ascii_digit()).unwrap_or(p.len());
        out = GoVersion { major, minor, stage: 3, n: number(&p[..end])? };
        rest = &p[end..];
    }
    if !rest.is_empty() {
        // `rc1`, `beta2`: lowercase letters, then digits.
        let letters = rest.find(|c: char| !c.is_ascii_lowercase()).unwrap_or(rest.len());
        let n = number(&rest[letters..])?;
        if letters == 0 || out.stage == 3 {
            return None;
        }
        let stage = if &rest[..letters] == "rc" { 2 } else { 1 };
        out = GoVersion { major, minor, stage, n };
    }
    Some(out)
}

/// The `go` and `toolchain` directives of a go.mod, as written.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GoMod {
    pub go: Option<String>,
    pub toolchain: Option<String>,
}

/// Go's rule for a go.mod without a `go` line: it means Go 1.16.
pub const GO_MOD_DEFAULT: (u32, u32) = (1, 16);

pub fn go_mod(text: &str) -> GoMod {
    let mut out = GoMod::default();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        let mut words = code.split_whitespace();
        let (Some(verb), Some(value), None) = (words.next(), words.next(), words.next()) else { continue };
        match verb {
            "go" if out.go.is_none() => out.go = Some(value.to_string()),
            "toolchain" if out.toolchain.is_none() => out.toolchain = Some(value.to_string()),
            _ => {}
        }
    }
    out
}

/// `use` folders of a go.work, as written.
pub fn go_work_uses(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        let spec = if in_block {
            if code == ")" {
                in_block = false;
                continue;
            }
            code
        } else {
            let Some(rest) = code.strip_prefix("use").filter(|r| r.starts_with([' ', '\t', '('])) else { continue };
            let rest = rest.trim_start();
            if rest == "(" {
                in_block = true;
                continue;
            }
            rest
        };
        let path = spec.trim().trim_matches('"');
        if !path.is_empty() {
            out.push(path.to_string());
        }
    }
    out
}

/// Local folders of go.mod `replace` directives (`=> ../lib`), as written.
pub fn go_mod_replaces(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        let spec = if in_block {
            if code == ")" {
                in_block = false;
                continue;
            }
            code
        } else {
            let Some(rest) = code.strip_prefix("replace").filter(|r| r.starts_with([' ', '\t', '('])) else { continue };
            let rest = rest.trim_start();
            if rest == "(" {
                in_block = true;
                continue;
            }
            rest
        };
        let Some((_, target)) = spec.split_once("=>") else { continue };
        let path = target.split_whitespace().next().unwrap_or("");
        if path == "." || path == ".." || ["./", "../", ".\\", "..\\"].iter().any(|p| path.starts_with(p)) {
            out.push(path.to_string());
        }
    }
    out
}

/// go.mod with the `go` line set to `version` (added after `module` when
/// missing) and every `toolchain` line removed; line endings are kept.
pub fn raise_go_mod(text: &str, version: &str) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(text.len() + 16);
    let mut done = false;
    let mut after_module: Option<usize> = None;
    for line in text.split_inclusive('\n') {
        let first = line.split("//").next().unwrap_or("").split_whitespace().next().unwrap_or("");
        match first {
            "toolchain" => continue,
            "go" if !done => {
                out.push_str("go ");
                out.push_str(version);
                out.push_str(eol);
                done = true;
                continue;
            }
            _ => {}
        }
        out.push_str(line);
        if first == "module" && after_module.is_none() {
            if !out.ends_with('\n') {
                out.push_str(eol);
            }
            after_module = Some(out.len());
        }
    }
    if !done {
        let line = format!("{eol}go {version}{eol}");
        if let Some(at) = after_module {
            out.insert_str(at, &line);
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str(eol);
            }
            out.push_str(line.trim_start_matches(['\r', '\n']));
        }
    }
    out
}

/// The `//go:build` expression of a Go file (before its `package` clause).
pub fn go_build_constraint(text: &str) -> Option<String> {
    for line in text.trim_start_matches('\u{feff}').lines() {
        let l = line.trim();
        if let Some(expr) = l.strip_prefix("//go:build") {
            if expr.starts_with([' ', '\t']) {
                return Some(expr.trim().to_string());
            }
            continue;
        }
        if l.starts_with("package ") || l == "package" {
            return None;
        }
    }
    None
}

// ---------- shared ----------

/// Leading `X.Y` of a version text (`3.12.1` → (3, 12), `20` → (20, 0)).
pub fn major_minor(v: &str) -> Option<(u32, u32)> {
    let mut parts = v.split(['.', '-', '+', ' ']);
    let a = parts.next()?.parse().ok()?;
    let b = parts.next().map_or(Some(0), |p| {
        let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() { Some(0) } else { digits.parse().ok() }
    })?;
    Some((a, b))
}

/// JSON with `//` and `/* */` comments and trailing commas (tsconfig,
/// global.json) made strict.
pub fn strip_json_comments(text: &str) -> String {
    let b = text.trim_start_matches('\u{feff}').as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
        } else if c == b'"' {
            in_str = true;
            out.push(c);
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if c == b',' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if !matches!(b.get(j), Some(b'}' | b']')) {
                out.push(c);
            }
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csproj_target_frameworks_and_condition() {
        let p = msbuild_props(
            "<Project><PropertyGroup><TargetFrameworkVersion>v4.8</TargetFrameworkVersion>\
             <TargetFrameworks>net8.0;net10.0-windows</TargetFrameworks>\
             <LangVersion Condition=\"'$(X)'=='1'\">12</LangVersion><LangVersion>latest</LangVersion>\
             </PropertyGroup></Project>",
        );
        assert_eq!(p["TargetFrameworks"], "net8.0;net10.0-windows");
        assert_eq!(p["LangVersion"], "latest");
        assert!(!p.contains_key("TargetFramework"));
        let (name, tfm) = lowest_tfm(&p).unwrap();
        assert_eq!(name, "net8.0");
        assert_eq!(tfm, Tfm::Core(8, 0));
    }

    #[test]
    fn directory_build_props_inheritance() {
        let outer = msbuild_props(
            "<Project><PropertyGroup><TargetFramework>net9.0</TargetFramework><LangVersion>13</LangVersion><AnalysisLevel>latest</AnalysisLevel></PropertyGroup></Project>",
        );
        let inner = msbuild_props(
            "<Project><Import Project=\"$([MSBuild]::GetPathOfFileAbove('Directory.Build.props', '$(MSBuildThisFileDirectory)../'))\" /><PropertyGroup><LangVersion>preview</LangVersion></PropertyGroup></Project>",
        );
        let project =
            msbuild_props("<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFrameworks>net10.0;net48</TargetFrameworks></PropertyGroup></Project>");
        assert!(imports_parent_props("<Import Project=\"$([MSBuild]::GetPathOfFileAbove('Directory.Build.props', '..'))\" />"));
        let merged = merge_props(&[outer, inner, project]);
        assert_eq!(merged["LangVersion"], "preview");
        assert_eq!(merged["AnalysisLevel"], "latest");
        assert_eq!(merged["TargetFrameworks"], "net10.0;net48");
        assert!(!merged.contains_key("TargetFramework"));
        assert_eq!(lowest_tfm(&merged).unwrap().1, Tfm::Framework(4, 8));
        assert!(!tfm_at_least(Tfm::Framework(4, 8), 6, 0));
        assert!(tfm_at_least(Tfm::Core(10, 0), 8, 0));
    }

    #[test]
    fn project_reference_paths() {
        let r = project_references(
            "<ItemGroup><ProjectReference Include=\"..\\A\\A.csproj\" /><ProjectReference Include='../B/B.csproj'></ProjectReference></ItemGroup>",
        );
        assert_eq!(r, vec!["..\\A\\A.csproj", "../B/B.csproj"]);
    }

    #[test]
    fn tfm_forms() {
        assert_eq!(parse_tfm("net10.0-windows"), Some(Tfm::Core(10, 0)));
        assert_eq!(parse_tfm("netcoreapp3.1"), Some(Tfm::Core(3, 1)));
        assert_eq!(parse_tfm("netstandard2.0"), Some(Tfm::Standard(2, 0)));
        assert_eq!(parse_tfm("net472"), Some(Tfm::Framework(4, 7)));
        assert_eq!(parse_tfm("$(Tfm)"), None);
    }

    #[test]
    fn global_json_with_comment() {
        let g = global_json("{\n // pinned\n \"sdk\": { \"version\": \"10.0.404\", \"rollForward\": \"disable\", }\n}");
        assert_eq!(g["sdk.version"], "10.0.404");
        assert_eq!(g["sdk.rollForward"], "disable");
    }

    #[test]
    fn cargo_workspace_inheritance() {
        let root = "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/old\"]\n[workspace.package]\nedition = \"2021\"\nrust-version = \"1.80\"\n";
        let member = "[package]\nname = \"a\"\nedition.workspace = true\nrust-version = { workspace = true }\n";
        let p = cargo_pins(member, Some(root)).unwrap();
        assert_eq!(p.name.as_deref(), Some("a"));
        assert_eq!(p.edition.as_deref(), Some("2021"));
        assert_eq!(p.rust_version.as_deref(), Some("1.80"));
        let w = cargo_pins(root, None).unwrap();
        assert!(w.is_workspace && !w.has_package);
        assert_eq!(w.edition.as_deref(), Some("2021"));
        assert!(is_member(&w, "crates/a"));
        assert!(!is_member(&w, "crates/old"));
        assert!(!is_member(&w, "tools/x"));
        let single = cargo_pins("[package]\nname = \"b\"\nedition = \"2024\"\n", None).unwrap();
        assert_eq!(single.edition.as_deref(), Some("2024"));
        assert_eq!(single.rust_version, None);
        assert!(cargo_pins("not toml [", None).is_err());
    }

    #[test]
    fn requires_python_forms() {
        assert_eq!(requires_python_min(">=3.12,<4"), Some((3, 12)));
        assert_eq!(requires_python_min(">= 3.9, != 3.9.1"), Some((3, 9)));
        assert_eq!(requires_python_min("~=3.11"), Some((3, 11)));
        assert_eq!(requires_python_min("==3.10.*"), Some((3, 10)));
        assert_eq!(requires_python_min("<3.13"), None);
        assert_eq!(python_version_file("# x\ncpython@3.13.1\n"), Some((3, 13)));
        assert_eq!(python_version_file("3.12\n"), Some((3, 12)));
    }

    #[test]
    fn engines_ranges() {
        assert_eq!(node_min(">=22 <27"), Some((22, 0, 0)));
        assert_eq!(node_min("^20.11.0"), Some((20, 11, 0)));
        assert_eq!(node_min("18.x || >=20"), Some((18, 0, 0)));
        assert_eq!(node_min("14 - 18"), Some((14, 0, 0)));
        assert_eq!(node_min(">= 16.14"), Some((16, 14, 0)));
        assert_eq!(node_min("<20"), Some((0, 0, 0)));
        assert_eq!(node_min("*"), Some((0, 0, 0)));
        assert_eq!(node_min("lts"), None);
    }

    #[test]
    fn baseline_from_browserslist() {
        assert_eq!(baseline_available(&["baseline 2023".into()]), serde_json::json!(2023));
        assert_eq!(baseline_available(&["baseline newly available".into()]), serde_json::json!("newly"));
        assert_eq!(baseline_available(&["> 0.5%".into(), "not dead".into()]), serde_json::json!("widely"));
        assert_eq!(browserslistrc("# c\n> 1%, last 2 versions\n[dev]\nlast 1 chrome version\n"), vec!["> 1%", "last 2 versions"]);
    }

    #[test]
    fn go_versions_and_order() {
        let v = |s: &str| go_version(s).unwrap();
        assert_eq!(v("1.26.0"), GoVersion { major: 1, minor: 26, stage: 3, n: 0 });
        assert_eq!(v("go1.27.4").minor_text(), "1.27");
        assert_eq!(v("1.22"), GoVersion::lang(1, 22));
        assert!(v("1.21") < v("1.21beta1") && v("1.21beta1") < v("1.21rc1") && v("1.21rc1") < v("1.21.0") && v("1.21.0") < v("1.21.1"));
        assert!(v("1.26rc1") < v("go1.27.4") && v("1.27.5") > v("go1.27.4") && v("1.9") < v("1.16"));
        assert_eq!(v("1.26rc1").minor_text(), "1.26");
        for junk in ["", "1", "1.x", "1.26.0.1", "1.021", "1.26rc", "1.26-rc1", "1.26.0rc1", "v1.26", "abc", "devel go1.28-abc"] {
            assert_eq!(go_version(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn go_mod_directives() {
        let m = go_mod("\u{feff}module example.com/a\r\n\r\ngo 1.26.0 // pinned\r\ntoolchain go1.27.1\r\n\r\nrequire (\r\n\tgo.uber.org/zap v1.27.0\r\n)\r\n");
        assert_eq!(m.go.as_deref(), Some("1.26.0"));
        assert_eq!(m.toolchain.as_deref(), Some("go1.27.1"));
        assert_eq!(go_mod("module x\n\nrequire golang.org/x/sys v0.1.0\n"), GoMod::default(), "missing go line");
        assert_eq!(go_mod("module x\ngo 1.26rc1\n").go.as_deref(), Some("1.26rc1"));
        assert_eq!(go_mod("module x\ngo 1.2x\n").go.and_then(|g| go_version(&g)), None, "junk stays unparsed");
        assert_eq!(go_mod("module x\n// go 1.30\ngo 1.21\n").go.as_deref(), Some("1.21"));
    }

    #[test]
    fn go_mod_local_replaces() {
        let text = "module x\nreplace example.com/a => ../a // local\nreplace (\n\texample.com/b v1.0.0 => ./third_party/b\n\texample.com/c => example.com/c2 v1.2.0\n)\nreplacement x\n";
        assert_eq!(go_mod_replaces(text), vec!["../a", "./third_party/b"]);
    }

    #[test]
    fn go_work_use_folders() {
        let text = "go 1.22\n\nuse ./app // main\nuse (\n\t./lib\n\t\"./with space\"\n)\nuser x\n";
        assert_eq!(go_work_uses(text), vec!["./app", "./lib", "./with space"]);
        assert_eq!(go_mod(&raise_go_mod("go 1.22\ntoolchain go1.23.0\nuse .\n", "1.27.4")).go.as_deref(), Some("1.27.4"), "go.work raises like go.mod");
    }

    #[test]
    fn go_mod_raise() {
        let r = raise_go_mod("module x\r\n\r\ngo 1.21\r\n\r\ntoolchain go1.22.3\r\n\r\nrequire go.uber.org/zap v1.27.0\r\n", "1.27.4");
        assert_eq!(r, "module x\r\n\r\ngo 1.27.4\r\n\r\n\r\nrequire go.uber.org/zap v1.27.0\r\n");
        let r = raise_go_mod("module x\nrequire (\n\tgolang.org/x/sys v0.1.0\n)\n", "1.24");
        assert_eq!(r, "module x\n\ngo 1.24\nrequire (\n\tgolang.org/x/sys v0.1.0\n)\n");
        assert_eq!(raise_go_mod("module x", "1.24"), "module x\n\ngo 1.24\n");
        assert_eq!(go_mod(&raise_go_mod("module x\ngo 1.16\n", "1.22")).go.as_deref(), Some("1.22"));
    }

    #[test]
    fn go_build_lines() {
        assert_eq!(go_build_constraint("// Copyright\n\n//go:build linux && amd64\n\npackage board\n").as_deref(), Some("linux && amd64"));
        assert_eq!(go_build_constraint("//go:build ignore\npackage main\n").as_deref(), Some("ignore"));
        assert_eq!(go_build_constraint("package a\n//go:build linux\n"), None, "after the package clause it is a comment");
        assert_eq!(go_build_constraint("//go:buildx linux\npackage a\n"), None);
    }

    #[test]
    fn json_comments_and_trailing_commas() {
        let v: serde_json::Value = serde_json::from_str(&strip_json_comments("{ \"a\": \"//x\", /* c */ \"b\": [1,2,], }")).unwrap();
        assert_eq!(v["a"], "//x");
        assert_eq!(v["b"][1], 2);
    }
}
