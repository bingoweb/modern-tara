//! The waiver file `modern-tara.toml`: findings a repository has agreed to
//! keep for now, each with a reason and optionally a card and an end date.
//!
//! ```toml
//! [[waiver]]
//! rule = "unicorn/prefer-module"      # exact rule id, or a prefix ending in `*`
//! paths = ["src/**/*.js", "bin/*.js"] # globs, relative to the top level
//! reason = "CommonJS until the ES-module series"
//! stack = "js"                        # optional
//! card = "MT20"                       # optional
//! expires = "2027-03-01"              # optional; the day itself still counts
//! ```

use crate::change::Glob;
use crate::report::Finding;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

/// The file's name at the repository top level.
pub const FILE: &str = "modern-tara.toml";

/// A calendar day `(year, month, day)`; tuples compare in date order.
pub type Day = (i32, u32, u32);

const KEYS: [&str; 6] = ["rule", "paths", "reason", "stack", "card", "expires"];

#[derive(Debug, Clone)]
pub struct Entry {
    /// 1-based, as in the file.
    pub index: usize,
    pub rule: String,
    pub paths: Vec<String>,
    pub stack: Option<String>,
    pub reason: String,
    pub card: Option<String>,
    /// The date as written, and parsed.
    pub expires: Option<(String, Day)>,
    /// `paths`, cleaned and compiled.
    globs: Vec<Glob>,
    /// Findings this entry waived.
    pub matched: usize,
}

impl Entry {
    /// `today` is past the last day the entry counts.
    pub fn expired(&self, today: Day) -> bool {
        self.expires.as_ref().is_some_and(|(_, day)| today > *day)
    }

    fn waives(&self, f: &Finding) -> bool {
        rule_matches(&self.rule, &f.rule) && self.stack.as_deref().is_none_or(|s| s == f.stack) && self.globs.iter().any(|g| g.matches(&f.file))
    }
}

/// The waivers of one scan: where they came from and the entries.
#[derive(Debug, Clone, Default)]
pub struct Set {
    /// `modern-tara.toml`, or the `--waivers` path; `None` without a file.
    pub file: Option<String>,
    pub entries: Vec<Entry>,
}

impl Set {
    /// `text` read from `name`.
    pub fn load(name: &str, text: &str) -> Result<Set, String> {
        Ok(Set { file: Some(name.to_string()), entries: parse(name, text)? })
    }

    /// Splits `findings` into the ones that stay and the waived ones (as JSON:
    /// the finding plus a `waiver` object). The first active entry that matches
    /// wins.
    pub fn apply(&mut self, findings: Vec<Finding>, today: Day) -> (Vec<Finding>, Vec<Value>) {
        let mut kept = Vec::new();
        let mut waived = Vec::new();
        for f in findings {
            match self.entries.iter().position(|e| !e.expired(today) && e.waives(&f)) {
                Some(at) => {
                    let e = &mut self.entries[at];
                    e.matched += 1;
                    let mut v = f.to_json();
                    if let Value::Object(m) = &mut v {
                        m.insert("waiver".into(), json!({"index": e.index, "rule": e.rule, "reason": e.reason, "card": e.card}));
                    }
                    waived.push(v);
                }
                None => kept.push(f),
            }
        }
        (kept, waived)
    }

    /// Warning texts: every expired entry, and (`all_lines` scans only, where
    /// every file is looked at) every active entry that matched nothing.
    pub fn warnings(&self, today: Day, all_lines: bool) -> Vec<String> {
        let mut out = Vec::new();
        for e in &self.entries {
            if let Some((text, _)) = e.expires.as_ref().filter(|_| e.expired(today)) {
                let card = e.card.as_ref().map_or_else(String::new, |c| format!(" (card {c})"));
                out.push(format!("waiver {} ({}) expired on {text}{card}; its findings count again", e.index, e.rule));
            } else if all_lines && e.matched == 0 {
                out.push(format!("waiver {} ({}) matched nothing", e.index, e.rule));
            }
        }
        out
    }

    /// The `waivers` object of the report.
    pub fn to_json(&self, today: Day) -> Value {
        let entries: Vec<Value> = self
            .entries
            .iter()
            .map(|e| {
                json!({
                    "index": e.index,
                    "rule": e.rule,
                    "paths": e.paths,
                    "stack": e.stack,
                    "reason": e.reason,
                    "card": e.card,
                    "expires": e.expires.as_ref().map(|(text, _)| text),
                    "status": if e.expired(today) { "expired" } else { "active" },
                    "matched": e.matched,
                })
            })
            .collect();
        json!({"file": self.file, "entries": entries})
    }
}

/// An exact rule id (ASCII case-insensitive) or a prefix ending in `*`.
fn rule_matches(pattern: &str, rule: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => rule.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()),
        None => pattern.eq_ignore_ascii_case(rule),
    }
}

/// What `modern-tara.toml` holds besides the waivers: facts about the files.
#[derive(Debug, Clone, Default)]
pub struct RepoFile {
    pub waivers: Set,
    /// `[js] classic-scripts`: `.js`/`.jsx` files that are browser scripts, not modules.
    pub classic_scripts: Vec<Glob>,
}

impl RepoFile {
    /// The repository's `modern-tara.toml`. `with_waivers` false (`--no-waivers`,
    /// `--waivers FILE`) skips the `[[waiver]]` list without reading it; `[js]`
    /// is always read.
    pub fn load(text: &str, with_waivers: bool) -> Result<RepoFile, String> {
        let part = if with_waivers { Part::Both } else { Part::JsOnly };
        let parsed = parse_file(FILE, text, part)?;
        let waivers = if with_waivers { Set { file: Some(FILE.to_string()), entries: parsed.entries } } else { Set::default() };
        Ok(RepoFile { waivers, classic_scripts: parsed.classic })
    }
}

/// Which parts of a file are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// The repository file: waivers and `[js]`.
    Both,
    /// The repository file when the waivers come from elsewhere or are off.
    JsOnly,
    /// A `--waivers FILE`: waivers only, `[js]` is refused.
    WaiversOnly,
}

struct Parsed {
    entries: Vec<Entry>,
    classic: Vec<Glob>,
}

fn parse(name: &str, text: &str) -> Result<Vec<Entry>, String> {
    parse_file(name, text, Part::WaiversOnly).map(|p| p.entries)
}

/// Errors read `<name>: waiver <n>: <what>` or `<name>: [js]: <what>`.
fn parse_file(name: &str, text: &str, part: Part) -> Result<Parsed, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("{name}: {e}"))?;
    if let Some(key) = table.keys().find(|k| !matches!(k.as_str(), "waiver" | "js")) {
        return Err(format!("{name}: unknown key `{key}` (only [[waiver]] entries and [js] are allowed)"));
    }
    let classic = match table.get("js") {
        None => Vec::new(),
        Some(_) if part == Part::WaiversOnly => return Err(format!("{name}: [js] belongs in the repository's modern-tara.toml")),
        Some(js) => classic_scripts(js).map_err(|e| format!("{name}: [js]: {e}"))?,
    };
    let mut entries = Vec::new();
    if part != Part::JsOnly
        && let Some(list) = table.get("waiver")
    {
        let list = list.as_array().ok_or_else(|| format!("{name}: `waiver` must be an array of tables ([[waiver]])"))?;
        entries = list.iter().enumerate().map(|(i, v)| entry(i + 1, v).map_err(|e| format!("{name}: waiver {}: {e}", i + 1))).collect::<Result<_, _>>()?;
    }
    Ok(Parsed { entries, classic })
}

/// The `[js]` table: `classic-scripts`, a non-empty array of globs.
fn classic_scripts(js: &toml::Value) -> Result<Vec<Glob>, String> {
    let table = js.as_table().ok_or("must be a table")?;
    if let Some(key) = table.keys().find(|k| k.as_str() != "classic-scripts") {
        return Err(format!("unknown key `{key}` (known: classic-scripts)"));
    }
    let Some(value) = table.get("classic-scripts") else { return Ok(Vec::new()) };
    let wrong = || "`classic-scripts` must be a non-empty array of non-empty strings".to_string();
    let list = value.as_array().filter(|l| !l.is_empty()).ok_or_else(wrong)?;
    list.iter()
        .map(|p| {
            let text = p.as_str().filter(|s| !s.trim().is_empty()).ok_or_else(wrong)?;
            compile_path("classic-scripts", text)
        })
        .collect()
}

/// A `paths`-style glob: cleaned, then compiled.
fn compile_path(key: &str, p: &str) -> Result<Glob, String> {
    let clean = clean_path(p).ok_or_else(|| format!("`{key}` \"{p}\" must be relative to the top level (no `..`, no drive or leading `/`)"))?;
    Glob::new(&clean).map_err(|e| format!("`{key}` \"{p}\": {e}"))
}

fn entry(index: usize, value: &toml::Value) -> Result<Entry, String> {
    let table = value.as_table().ok_or("must be a table")?;
    if let Some(key) = table.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown key `{key}` (known: {})", KEYS.join(", ")));
    }
    let rule = string(table, "rule")?.ok_or("`rule` is required")?;
    let stars = rule.matches('*').count();
    if stars > 1 || (stars == 1 && (!rule.ends_with('*') || rule.len() == 1)) {
        return Err(format!("`rule` \"{rule}\" is not an exact id or a prefix ending in `*`"));
    }
    let paths = paths(table)?;
    let reason = string(table, "reason")?.ok_or("`reason` is required")?;
    let stack = string(table, "stack")?;
    if let Some(s) = stack.as_deref().filter(|s| !crate::stacks::ALL.contains(s)) {
        return Err(format!("`stack` \"{s}\" is not one of {}", crate::stacks::ALL.join(", ")));
    }
    let card = string(table, "card")?;
    let expires = match string(table, "expires")? {
        Some(text) => {
            let day = parse_day(&text).ok_or_else(|| format!("`expires` \"{text}\" is not a calendar date (YYYY-MM-DD)"))?;
            Some((text, day))
        }
        None => None,
    };
    if rule.ends_with('*') && stack.is_none() {
        return Err("a rule prefix needs `stack`".into());
    }
    let globs = paths.iter().map(|p| compile_path("paths", p)).collect::<Result<Vec<_>, _>>()?;
    Ok(Entry { index, rule, paths, stack, reason, card, expires, globs, matched: 0 })
}

/// A `paths` glob as `--files` reads one: `\` is `/`, a leading `./` goes.
/// `None` for an absolute path or one with a `..` segment.
fn clean_path(raw: &str) -> Option<String> {
    let slashed = raw.replace('\\', "/");
    let mut p = slashed.as_str();
    while let Some(rest) = p.strip_prefix("./") {
        p = rest.trim_start_matches('/');
    }
    let drive = p.as_bytes().get(1) == Some(&b':') && p.as_bytes()[0].is_ascii_alphabetic();
    (!p.starts_with('/') && !drive && !p.split('/').any(|seg| seg == "..")).then(|| p.to_string())
}

/// An optional string key: absent is `None`; a non-string or an empty one is an error.
fn string(table: &toml::Table, key: &str) -> Result<Option<String>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) if s.trim().is_empty() => Err(format!("`{key}` must not be empty")),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn paths(table: &toml::Table) -> Result<Vec<String>, String> {
    let list = table.get("paths").ok_or("`paths` is required")?.as_array().ok_or("`paths` must be an array of strings")?;
    if list.is_empty() {
        return Err("`paths` must not be empty".into());
    }
    list.iter()
        .map(|p| match p {
            toml::Value::String(s) if s.trim().is_empty() => Err("`paths` must not hold an empty string".to_string()),
            toml::Value::String(s) => Ok(s.clone()),
            _ => Err("`paths` must be an array of strings".to_string()),
        })
        .collect()
}

/// `YYYY-MM-DD`, a real calendar day.
pub fn parse_day(text: &str) -> Option<Day> {
    let b = text.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' || !b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()) {
        return None;
    }
    let (y, m, d): (i32, u32, u32) = (text[..4].parse().ok()?, text[5..7].parse().ok()?, text[8..].parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // A day that does not exist rolls over into the next month.
    (civil_from_days(days_from_civil(i64::from(y), m, d)) == (y, m, d)).then_some((y, m, d))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (`m` 1..=12).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of `days` since 1970-01-01.
pub fn civil_from_days(days: i64) -> Day {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (i32::try_from(y).unwrap_or(i32::MAX), u32::try_from(m).unwrap_or(0), u32::try_from(d).unwrap_or(0))
}

/// Today's date in UTC. A clock that fails or reads before 1970 gives the
/// far future, so every expiry counts as passed (fail closed).
pub fn today_utc() -> Day {
    day_at(SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()))
}

/// The date of `secs` since the epoch; `None` (no usable clock) is the far future.
fn day_at(secs: Option<u64>) -> Day {
    secs.and_then(|s| i64::try_from(s / 86_400).ok()).map_or(FAR_FUTURE, civil_from_days)
}

const FAR_FUTURE: Day = (9999, 12, 31);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Confidence, Kind};

    const TODAY: Day = (2026, 11, 22);

    fn finding(stack: &'static str, file: &str, rule: &str) -> Finding {
        Finding {
            stack,
            file: file.into(),
            line: 1,
            column: 1,
            rule: rule.into(),
            message: "m".into(),
            kind: Kind::Modern,
            confidence: Confidence::High,
            source: "tool",
            upgrade: None,
        }
    }

    fn one(extra: &str) -> String {
        format!("[[waiver]]\nrule = \"UP006\"\npaths = [\"src/**/*.py\"]\nreason = \"later\"\n{extra}")
    }

    fn set(text: &str) -> Set {
        Set::load(FILE, text).unwrap()
    }

    #[test]
    fn parses_a_full_file() {
        let text = "[[waiver]]\nrule = \"unicorn/prefer-module\"\npaths = [\"src/**/*.js\", \"bin/*.js\"]\nreason = \"CommonJS until the ES-module series\"\n\
                    stack = \"js\"\ncard = \"MT20\"\nexpires = \"2027-03-01\"\n\n[[waiver]]\nrule = \"UP*\"\npaths = [\"*.py\"]\nreason = \"x\"\nstack = \"python\"\n";
        let s = set(text);
        assert_eq!(s.file.as_deref(), Some("modern-tara.toml"));
        assert_eq!(s.entries.len(), 2);
        let e = &s.entries[0];
        assert_eq!((e.index, e.rule.as_str(), e.stack.as_deref(), e.card.as_deref()), (1, "unicorn/prefer-module", Some("js"), Some("MT20")));
        assert_eq!(e.paths, vec!["src/**/*.js", "bin/*.js"]);
        assert_eq!(e.expires, Some(("2027-03-01".to_string(), (2027, 3, 1))));
        assert_eq!(s.entries[1].index, 2);
        assert!(s.entries[1].stack.as_deref() == Some("python") && s.entries[1].expires.is_none());
        assert!(set("").entries.is_empty(), "an empty file has no waivers");
        assert!(set("# nothing here\n").entries.is_empty());
    }

    #[test]
    fn every_error_names_the_entry() {
        let bad = |text: &str, want: &str| {
            let err = Set::load(FILE, text).unwrap_err();
            assert!(err.starts_with("modern-tara.toml: "), "{err}");
            assert!(err.contains(want), "wanted `{want}` in `{err}`");
        };
        let ok_first = one("");
        let second = |body: &str| format!("{ok_first}\n[[waiver]]\n{body}");
        bad("[tool]\nx = 1\n", "unknown key `tool`");
        bad("waiver = 3\n", "array of tables");
        bad("[waiver]\nrule = \"a\"\n", "array of tables");
        bad("[[waiver]\n", "modern-tara.toml:");
        bad(&second("rule = \"A\"\npaths = [\"a\"]\nreason = \"r\"\nowner = \"x\"\n"), "waiver 2: unknown key `owner`");
        bad(&second("paths = [\"a\"]\nreason = \"r\"\n"), "waiver 2: `rule` is required");
        bad(&second("rule = \"A\"\nreason = \"r\"\n"), "waiver 2: `paths` is required");
        bad(&second("rule = \"A\"\npaths = [\"a\"]\n"), "waiver 2: `reason` is required");
        bad(&second("rule = 5\npaths = [\"a\"]\nreason = \"r\"\n"), "waiver 2: `rule` must be a string");
        bad(&second("rule = \"\"\npaths = [\"a\"]\nreason = \"r\"\n"), "waiver 2: `rule` must not be empty");
        bad(&second("rule = \"A\"\npaths = []\nreason = \"r\"\n"), "waiver 2: `paths` must not be empty");
        bad(&second("rule = \"A\"\npaths = [\"\"]\nreason = \"r\"\n"), "waiver 2: `paths` must not hold an empty string");
        bad(&second("rule = \"A\"\npaths = [1]\nreason = \"r\"\n"), "waiver 2: `paths` must be an array of strings");
        bad(&second("rule = \"A\"\npaths = \"a\"\nreason = \"r\"\n"), "waiver 2: `paths` must be an array of strings");
        bad(&second("rule = \"A\"\npaths = [\"a\"]\nreason = \"\"\n"), "waiver 2: `reason` must not be empty");
        bad(&second("rule = \"A\"\npaths = [\"a\"]\nreason = \"r\"\nstack = \"cobol\"\n"), "waiver 2: `stack` \"cobol\" is not one of");
        bad(&second("rule = \"A\"\npaths = [\"a\"]\nreason = \"r\"\ncard = \"\"\n"), "waiver 2: `card` must not be empty");
        bad(&second("rule = \"A\"\npaths = [\"a\"]\nreason = \"r\"\nexpires = 2027-03-01\n"), "waiver 2: `expires` must be a string");
        bad(&second("rule = \"A\"\npaths = [\"a\"]\nreason = \"r\"\nexpires = \"soon\"\n"), "waiver 2: `expires` \"soon\"");
        bad(&second("rule = \"A\"\npaths = [\"a\"]\nreason = \"r\"\nexpires = \"2026-02-30\"\n"), "waiver 2: `expires` \"2026-02-30\"");
        let over = "{a,b}".repeat(9);
        bad(&second(&format!("rule = \"A\"\npaths = [\"{over}\"]\nreason = \"r\"\n")), "waiver 2: `paths`");
        for rule in ["*", "a*b", "**", "a**", "*a"] {
            bad(&one("").replace("UP006", rule), &format!("waiver 1: `rule` \"{rule}\""));
        }
        bad(&second("rule = \"UP*\"\npaths = [\"a\"]\nreason = \"r\"\n"), "waiver 2: a rule prefix needs `stack`");
        assert!(Set::load(FILE, &second("rule = \"UP*\"\npaths = [\"a\"]\nreason = \"r\"\nstack = \"python\"\n")).is_ok());
        // Whitespace alone is empty.
        bad(&second("rule = \"A\"\npaths = [\"a\"]\nreason = \"   \"\n"), "waiver 2: `reason` must not be empty");
        bad(&second("rule = \"A\"\npaths = [\" \"]\nreason = \"r\"\n"), "waiver 2: `paths` must not hold an empty string");
        // Paths stay inside the top level.
        for path in ["../x", "a/../b.py", "..\\\\x", "/abs/x.py", "C:/x/y.py", "C:\\\\x.py", "./../x"] {
            bad(&second(&format!("rule = \"A\"\npaths = [\"{path}\"]\nreason = \"r\"\n")), "waiver 2: `paths`");
        }
    }

    #[test]
    fn paths_are_cleaned_like_files_patterns() {
        let s = set("[[waiver]]\nrule = \"R\"\npaths = [\"./src/*.py\", \"bin\\\\*.py\", \".//lib/**\"]\nreason = \"r\"\n");
        let hits = |file: &str| s.entries[0].waives(&finding("python", file, "R"));
        assert!(hits("src/a.py"), "a leading ./ is dropped");
        assert!(hits("bin/b.py"), "a backslash is a slash");
        assert!(hits("lib/x/y.py"));
        assert!(!hits("other/a.py") && !hits("src/sub/a.py"));
        // Reported as written.
        assert_eq!(s.entries[0].paths[1], "bin\\*.py");
    }

    #[test]
    fn js_classic_scripts_table() {
        let text = format!("[js]\nclassic-scripts = [\"public/**/*.js\", \"./kiosk/*.{{js,jsx}}\", \"old\\\\*.js\"]\n\n{}", one(""));
        let repo = RepoFile::load(&text, true).unwrap();
        let hit = |p: &str| repo.classic_scripts.iter().any(|g| g.matches(p));
        assert!(hit("public/app.js") && hit("public/a/b/c.js") && hit("kiosk/x.jsx") && hit("old/z.js"));
        assert!(!hit("src/app.js") && !hit("public/app.ts") && !hit("kiosk/a/x.js"));
        assert_eq!(repo.waivers.file.as_deref(), Some("modern-tara.toml"));
        assert_eq!(repo.waivers.entries.len(), 1);
        // No `[js]` table: no classic scripts.
        assert!(RepoFile::load(&one(""), true).unwrap().classic_scripts.is_empty());
        assert!(RepoFile::load("[js]\n", true).unwrap().classic_scripts.is_empty());
    }

    #[test]
    fn js_table_errors() {
        let bad = |text: &str, want: &str| {
            let err = RepoFile::load(text, true).unwrap_err();
            assert!(err.starts_with("modern-tara.toml: [js]: "), "{err}");
            assert!(err.contains(want), "wanted `{want}` in `{err}`");
        };
        bad("[js]\nmodules = [\"a\"]\n", "unknown key `modules`");
        bad("[js]\nclassic-scripts = []\n", "non-empty array of non-empty strings");
        bad("[js]\nclassic-scripts = \"a.js\"\n", "non-empty array of non-empty strings");
        bad("[js]\nclassic-scripts = [1]\n", "non-empty array of non-empty strings");
        bad("[js]\nclassic-scripts = [\"\"]\n", "non-empty array of non-empty strings");
        bad("[js]\nclassic-scripts = [\"  \"]\n", "non-empty array of non-empty strings");
        bad("[js]\nclassic-scripts = [\"../x/*.js\"]\n", "`classic-scripts` \"../x/*.js\"");
        bad("[js]\nclassic-scripts = [\"/abs/*.js\"]\n", "relative to the top level");
        bad(&format!("[js]\nclassic-scripts = [\"{}\"]\n", "{a,b}".repeat(9)), "256");
        bad("js = 3\n", "must be a table");
        // Still an error with the waiver list off: `[js]` is always read.
        assert!(RepoFile::load("[js]\nclassic-scripts = []\n", false).is_err());
    }

    #[test]
    fn the_waiver_list_is_skipped_unread_when_off_and_js_is_refused_in_a_waivers_file() {
        let broken = "[js]\nclassic-scripts = [\"p/*.js\"]\n\n[[waiver]]\nrule = \"R\"\nbogus = 1\n";
        assert!(RepoFile::load(broken, true).is_err());
        let off = RepoFile::load(broken, false).unwrap();
        assert!(off.waivers.file.is_none() && off.waivers.entries.is_empty());
        assert_eq!(off.classic_scripts.len(), 1, "--no-waivers keeps classic-scripts");
        let err = Set::load("ci/waivers.toml", broken).unwrap_err();
        assert_eq!(err, "ci/waivers.toml: [js] belongs in the repository's modern-tara.toml");
        let err = Set::load("ci/waivers.toml", "[tool]\nx = 1\n").unwrap_err();
        assert!(err.contains("unknown key `tool`"), "{err}");
    }

    #[test]
    fn no_clock_fails_closed() {
        assert_eq!(day_at(Some(0)), (1970, 1, 1));
        assert_eq!(day_at(Some(20_779 * 86_400 + 86_399)), (2026, 11, 22));
        assert_eq!(day_at(None), (9999, 12, 31));
        let s = set(&one("expires = \"2099-01-01\"\n"));
        assert!(!s.entries[0].expired(day_at(Some(0))));
        assert!(s.entries[0].expired(day_at(None)), "no clock: every expiry has passed");
        assert!(!set(&one("")).entries[0].expired(day_at(None)), "no expiry never expires");
    }

    #[test]
    fn prefix_and_exact_rules_ignore_case() {
        assert!(rule_matches("unicorn/prefer-*", "unicorn/prefer-module"));
        assert!(rule_matches("UNICORN/PREFER-*", "unicorn/prefer-module"));
        assert!(!rule_matches("unicorn/prefer-*", "unicorn/no-null"));
        assert!(rule_matches("UP*", "UP006") && rule_matches("up006", "UP006") && !rule_matches("UP00", "UP006"));
        assert!(!rule_matches("UP006", "UP0061"));
    }

    #[test]
    fn stack_filter() {
        let mut s = set(&one("stack = \"python\"\n"));
        let (kept, waived) = s.apply(vec![finding("python", "src/a.py", "UP006"), finding("js", "src/a.py", "UP006")], TODAY);
        assert_eq!(waived.len(), 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].stack, "js");
    }

    #[test]
    fn paths_use_the_globs_of_the_scanner() {
        let mut s = set("[[waiver]]\nrule = \"R\"\npaths = [\"{src,lib}/**/[!_]*.py\", \"top.py\"]\nreason = \"r\"\n");
        let all = vec![
            finding("python", "src/a/b.py", "R"),
            finding("python", "lib/c.py", "R"),
            finding("python", "src/_p.py", "R"),
            finding("python", "top.py", "R"),
            finding("python", "other/x.py", "R"),
        ];
        let (kept, waived) = s.apply(all, TODAY);
        assert_eq!(waived.len(), 3);
        assert_eq!(kept.iter().map(|f| f.file.as_str()).collect::<Vec<_>>(), vec!["src/_p.py", "other/x.py"]);
    }

    #[test]
    fn first_active_match_wins() {
        let text = format!(
            "{}\n[[waiver]]\nrule = \"UP*\"\nstack = \"python\"\npaths = [\"**/*.py\"]\nreason = \"broad\"\ncard = \"MT9\"\n\n[[waiver]]\nrule = \"UP006\"\npaths = [\"**\"]\nreason = \"third\"\n",
            one("expires = \"2020-01-01\"\n")
        );
        let mut s = set(&text);
        let (kept, waived) = s.apply(vec![finding("python", "src/a.py", "UP006")], TODAY);
        assert!(kept.is_empty());
        assert_eq!(waived.len(), 1);
        // Entry 1 matches first but is expired: entry 2 waives.
        assert_eq!(waived[0]["waiver"], json!({"index": 2, "rule": "UP*", "reason": "broad", "card": "MT9"}));
        assert_eq!(waived[0]["rule"], "UP006");
        assert_eq!(waived[0]["file"], "src/a.py");
        assert_eq!(s.entries.iter().map(|e| e.matched).collect::<Vec<_>>(), vec![0, 1, 0]);
        let doc = s.to_json(TODAY);
        assert_eq!(doc["file"], "modern-tara.toml");
        assert_eq!(doc["entries"][0]["status"], "expired");
        assert_eq!(doc["entries"][1]["status"], "active");
        assert_eq!(doc["entries"][1]["matched"], 1);
        assert_eq!(doc["entries"][0]["expires"], "2020-01-01");
        assert!(doc["entries"][1]["expires"].is_null() && doc["entries"][2]["stack"].is_null());
        assert_eq!(doc["entries"][1]["stack"], "python");
        assert_eq!(doc["entries"][1]["paths"], json!(["**/*.py"]));
    }

    #[test]
    fn expiry_day_still_counts() {
        let s = set(&one("expires = \"2026-11-22\"\n"));
        let e = &s.entries[0];
        assert!(!e.expired((2026, 11, 21)));
        assert!(!e.expired((2026, 11, 22)), "today == expires is active");
        assert!(e.expired((2026, 11, 23)));
        assert!(e.expired((2027, 1, 1)) && !e.expired((2026, 1, 1)));
        let mut s = s;
        let (kept, waived) = s.apply(vec![finding("python", "src/a.py", "UP006")], (2026, 11, 23));
        assert_eq!((kept.len(), waived.len()), (1, 0), "expired waives nothing");
        assert!(set(&one("")).entries[0].expires.is_none());
        assert!(!set(&one("")).entries[0].expired((9999, 12, 31)));
    }

    #[test]
    fn warnings_for_expired_and_unmatched() {
        let text = format!("{}\n[[waiver]]\nrule = \"X\"\npaths = [\"nope/*\"]\nreason = \"r\"\n", one("expires = \"2020-01-01\"\ncard = \"MT20\"\n"));
        let s = set(&text);
        assert_eq!(s.warnings(TODAY, false), vec!["waiver 1 (UP006) expired on 2020-01-01 (card MT20); its findings count again"]);
        assert_eq!(
            s.warnings(TODAY, true),
            vec!["waiver 1 (UP006) expired on 2020-01-01 (card MT20); its findings count again", "waiver 2 (X) matched nothing"]
        );
        let plain = set(&one("expires = \"2020-01-01\"\n"));
        assert_eq!(plain.warnings(TODAY, false), vec!["waiver 1 (UP006) expired on 2020-01-01; its findings count again"]);
    }

    #[test]
    fn no_file_serializes_empty() {
        let doc = Set::default().to_json(TODAY);
        assert!(doc["file"].is_null());
        assert_eq!(doc["entries"], json!([]));
    }

    #[test]
    fn civil_dates_of_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_779), (2026, 11, 22));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
        assert_eq!(days_from_civil(2026, 11, 22), 20_779);
        for days in [-800_000, -1, 0, 59, 60, 11_016, 11_017, 20_779, 2_000_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(i64::from(y), m, d), days);
        }
        assert!(today_utc().0 >= 2026);
    }

    #[test]
    fn day_parsing_checks_the_calendar() {
        assert_eq!(parse_day("2026-11-22"), Some((2026, 11, 22)));
        assert_eq!(parse_day("2000-02-29"), Some((2000, 2, 29)));
        assert_eq!(parse_day("2028-02-29"), Some((2028, 2, 29)));
        for bad in [
            "2026-02-30",
            "2026-13-01",
            "2026-00-10",
            "2026-04-31",
            "1900-02-29",
            "2026-02-29",
            "2026-1-1",
            "26-11-22",
            "2026/11/22",
            "2026-11-22 ",
            "",
            "2026-11-0x",
            "2026-11-00",
        ] {
            assert_eq!(parse_day(bad), None, "{bad}");
        }
    }
}
