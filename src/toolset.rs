//! `tools.toml`: the exact version of every external tool modern-tara runs.
//! `--version` compares it with what is installed; the install scripts
//! (`scripts/install-tools.{ps1,sh}`) read the same file.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ToolSet {
    /// `[tools]`: ruff, ast-grep, `PSScriptAnalyzer`.
    pub tools: BTreeMap<String, String>,
    /// `[npm]`: the packages `js/package.json` pins.
    pub npm: BTreeMap<String, String>,
    /// `[sha256]`: download (release asset or nupkg) name to SHA-256.
    pub sha256: BTreeMap<String, String>,
}

/// `tools.toml` in the modern-tara folder.
pub fn load(home: &Path) -> Result<ToolSet, String> {
    let path = home.join("tools.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text)
}

pub fn parse(text: &str) -> Result<ToolSet, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("tools.toml: {e}"))?;
    let section = |name: &str| -> Result<BTreeMap<String, String>, String> {
        let Some(v) = table.get(name) else { return Ok(BTreeMap::new()) };
        let t = v.as_table().ok_or_else(|| format!("tools.toml: [{name}] is not a table"))?;
        t.iter().map(|(k, x)| x.as_str().map(|s| (k.clone(), s.to_string())).ok_or_else(|| format!("tools.toml: {name}.{k} is not a string"))).collect()
    };
    let set = ToolSet { tools: section("tools")?, npm: section("npm")?, sha256: section("sha256")? };
    if set.tools.is_empty() {
        return Err("tools.toml: [tools] is missing or empty".into());
    }
    for (name, version) in set.tools.iter().chain(&set.npm) {
        if !is_exact(version) {
            return Err(format!("tools.toml: {name} = \"{version}\" is not an exact version (x.y.z)"));
        }
    }
    for (asset, hash) in &set.sha256 {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("tools.toml: SHA-256 of {asset} is not 64 hex digits"));
        }
    }
    Ok(set)
}

fn is_exact(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The version in a `--version` line: the first word that starts with a
/// digit, without a leading `v` (`ruff 0.17.6`, `v26.1.0`, `1.25.0`).
pub fn version_in(line: &str) -> Option<&str> {
    line.split_whitespace()
        .map(|w| w.trim_prefix('v'))
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        .map(|w| w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()))
}

/// A warning line when `installed` (a `--version` line, or `not found`)
/// is not the pinned version; `None` when they match.
pub fn check(name: &str, pinned: &str, installed: &str) -> Option<String> {
    match version_in(installed) {
        Some(v) if v == pinned => None,
        Some(v) => Some(format!("warning: {name}: {v} installed; tools.toml pins {pinned}")),
        None => Some(format!("warning: {name}: {}; tools.toml pins {pinned}", installed.trim())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_and_rejects_ranges() {
        assert!(parse("[tools]\nruff = \"0.17.6\"\n[sha256]\n\"app.zip\" = \"").is_err(), "unterminated string must fail");
        let hash = "a".repeat(64);
        let text = format!("[tools]\nruff = \"0.17.6\"\nast-grep = \"0.51.3\"\n[npm]\n\"@eslint/css\" = \"2.0.1\"\n[sha256]\n\"app.zip\" = \"{hash}\"\n");
        let set = parse(&text).unwrap();
        assert_eq!(set.tools["ast-grep"], "0.51.3");
        assert_eq!(set.npm["@eslint/css"], "2.0.1");
        assert_eq!(set.sha256["app.zip"], hash);
        assert!(parse("[tools]\nruff = \"^0.17\"\n").is_err());
        assert!(parse("[tools]\nruff = \"0.17\"\n").is_err());
        assert!(parse("[npm]\neslint = \"10.15.0\"\n").is_err(), "no [tools]");
        assert!(parse("[tools]\nruff = \"0.17.6\"\n[sha256]\nx = \"abc\"\n").is_err());
    }

    /// The shipped `tools.toml`: every tool the install scripts fetch, a
    /// SHA-256 for each download they verify, and the npm set of `js/package.json`.
    #[test]
    fn repository_tools_toml_is_complete_and_matches_package_json() {
        let set = parse(include_str!("../tools.toml")).unwrap();
        for tool in ["ruff", "ast-grep", "PSScriptAnalyzer"] {
            assert!(set.tools.contains_key(tool), "[tools] lacks {tool}");
        }
        let pkg: serde_json::Value = serde_json::from_str(include_str!("../js/package.json")).unwrap();
        let deps: BTreeMap<String, String> =
            pkg["devDependencies"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        assert_eq!(set.npm, deps, "[npm] in tools.toml and js/package.json devDependencies differ");
        let mut downloads = vec![format!("PSScriptAnalyzer.{}.nupkg", set.tools["PSScriptAnalyzer"])];
        for (target, archive) in [("x86_64-pc-windows-msvc", "zip"), ("x86_64-unknown-linux-gnu", "tar.gz")] {
            downloads.push(format!("app-{target}.zip"));
            downloads.push(format!("ruff-{target}.{archive}"));
        }
        for name in &downloads {
            assert!(set.sha256.contains_key(name), "[sha256] lacks {name}");
        }
    }

    #[test]
    fn compares_installed_lines() {
        assert_eq!(version_in("ruff 0.17.6"), Some("0.17.6"));
        assert_eq!(version_in("ast-grep 0.51.3"), Some("0.51.3"));
        assert_eq!(version_in("v26.1.0"), Some("26.1.0"));
        assert_eq!(version_in("not found"), None);
        assert_eq!(check("ruff", "0.17.6", "ruff 0.17.6"), None);
        assert_eq!(check("ruff", "0.17.6", "ruff 0.17.5").as_deref(), Some("warning: ruff: 0.17.5 installed; tools.toml pins 0.17.6"));
        assert_eq!(check("eslint", "10.15.0", "not installed").as_deref(), Some("warning: eslint: not installed; tools.toml pins 10.15.0"));
    }
}
