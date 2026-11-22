# modern-tara usage

modern-tara finds old constructs on the **changed lines** of a repository, per
stack, and says which ones need a newer toolchain or pin. "Modern" means the
newest stable release (STS included). Every tool runs on a temp copy
(`<temp>/modern-tara-<pid>-<n>`, removed on exit); nothing is written into the
scanned repository.

## What it runs, with your rights

The Rust and C# stacks **build repository code** in the temp copy. That means
`build.rs` scripts, procedural macros and MSBuild/NuGet targets, and they run
with the rights of the user who started modern-tara. `--git-safe` only trusts
the repository's ownership for git; it does not sandbox anything. Do not point
modern-tara at code you would not build yourself.

The copy never includes `target/`, `bin/` or `obj/` that are build output:
next to a `Cargo.toml` or .NET project, and untracked. Tracked sources under
such names (`src/bin/*.rs`) are copied.

Side effects outside the repository:

- A restore or build fills the global `~/.nuget/packages` and `~/.cargo/registry`
  caches, which can mean downloads.
- The Rust target cache lives in `<OS cache folder>/modern-tara/cache/<hash>`
  (see [Cache](#cache)). Each folder's `source.txt` names its repository.
- Every child process inherits `RUSTUP_AUTO_INSTALL=0`, and version probes run
  from the temp folder, so a project's `rust-toolchain.toml` never makes rustup
  install anything.

## Command line

```
modern-tara scan [--repo <dir>] [--range <a>..<b> | --staged] [--files <f>...] [--all-lines]
                 [--stacks csharp,rust,python,js,css,powershell] [--json <out.json>] [--quiet]
                 [--git-safe] [--timeout <seconds>] [--gate]
modern-tara --version     # this tool, every underlying tool, and tools.toml mismatches
```

| change source | lines | file contents |
|---|---|---|
| `--range a..b` | `git diff -U0 a b` | revision `b` (`git cat-file --batch`), never the work tree |
| `--staged` | `git diff -U0 --cached HEAD` | the index |
| neither | `git diff -U0 HEAD`, untracked files whole | the work tree |
| `--files f... --all-lines` | whole files (globs allowed, also outside git) | work tree / folder |
| `--all-lines` alone | every tracked file | as above |

- stdout: JSON (or `--json FILE`); stderr: one line per stack, a total line,
  and with `--gate` a final gate line. `--quiet` drops all of stderr.
- `--repo <subdir>` without `--files` limits the change to that subfolder.
- `--git-safe` adds `-c safe.directory=<repo>` to each git call (for
  repositories owned by another user). Git always runs with
  `--no-optional-locks` and fsmonitor off.
- `--timeout` (and each default) applies **per child process**, not per stack.

### Exit codes and the gate

| exit | without `--gate` | with `--gate` |
|---|---|---|
| `0` | no findings | no finding with `kind: "modern"` and `confidence: "high"` (other findings are warnings) |
| `1` | at least one finding (any kind) | at least one `modern` + `high` finding |
| `2` | tool error, or a stack failed and nothing was found | tool error, or a stack failed and no `modern` + `high` finding was found |

`needs-newer`, `baseline` and every `low` finding never stop the gate; they
are still in the JSON and in the counts. With `--gate` the last stderr line is
`modern-tara: gate passed` (exit 0) or
`modern-tara: gate stopped (<n> high-confidence modern findings)` (exit 1);
exit 2 prints no gate line.

The summary counts findings per kind and confidence, per stack and in total:

```
python     ok        1 files  2 modern high, 1 needs-newer low                0.4s
modern-tara: 3 findings (2 modern high, 1 needs-newer low) in 0.6s
modern-tara: gate stopped (2 high-confidence modern findings)
```

## JSON

```json
{ "tool": "modern-tara", "version": "0.2.0", "repo": "C:/...",
  "change": { "mode": "range|staged|worktree|files", "from", "to", "all_lines", "files", "changed_lines", "git_safe" },
  "stacks": [ { "stack", "files": [], "pins": {}, "newest": {}, "tool": "ruff 0.17.6",
                "status": "ok|skipped|error", "message", "duration_ms" } ],
  "findings": [ { "stack", "file", "line", "column", "rule", "message",
                  "kind": "modern|needs-newer|baseline", "confidence": "high|low",
                  "source": "tool|ast-grep",
                  "upgrade": { "pin", "from", "to", "note" } | null } ] }
```

Paths are repo-relative with `/`. Kinds:

- **modern**: an old form whose modern replacement works under the current pins.
- **needs-newer**: works only after an upgrade; `upgrade` names the pin.
- **baseline**: the target may not support the code (CSS Baseline, ES2022+
  features above `engines.node`, PowerShell 5.1 syntax).

Confidence:

- **high**: every finding of a rule engine (ruff, clippy, `dotnet format`,
  ESLint, stylelint, @eslint/css, PSScriptAnalyzer), and ast-grep catalogue
  rules whose pattern is the old form itself.
- **low**: ast-grep catalogue heuristics, where the old form is often right
  where it is. The level is `metadata.confidence` in each `rules/*.yml` rule
  (a rule without it counts as `low`). Low today: `csharp-datetime-now`,
  `csharp-httpclient-per-call`, `csharp-new-random`, `python-asyncio-gather`,
  `javascript-http-client-package` and its `typescript-` and `tsx-` twins.
  Also low: stylelint `declaration-block-no-redundant-longhand-properties`
  (see [Rule selection](#rule-selection)).

A missing tool sets the stack to `skipped`, and `message` says what to install.

## Rule selection

A rule is run only when it moves code to a newer language or runtime
construct. Pure style rules are left out:

| stack | left out | why |
|---|---|---|
| PowerShell | `PSAvoidUsingPositionalParameters` | parameter names are style; on a real repository it was 497 of 1069 findings |
| PowerShell | `PSAvoidUsingCmdletAliases` | alias vs. full cmdlet name is style |
| PowerShell | `PSAvoidUsingWriteHost` | output streams, not age |
| ESLint core | `prefer-const` | `let` and `const` are the same ES2015 generation |
| stylelint | `alpha-value-notation` | number vs. percentage alpha are both current |
| stylelint | `shorthand-property-no-redundant-values` | redundancy, not age |
| JS (unicorn) | `prefer-await` | low precision (kept promises, fire-and-forget calls) |

Kept from the reviewed sets: `PSAvoidUsingWMICmdlet` (WMI → CIM cmdlets),
`PSUseCompatibleSyntax` (baseline), ESLint `no-var`, `prefer-template`,
`object-shorthand` (ES2015 constructs), `prefer-object-has-own` (ES2022),
stylelint `color-function-notation`, `media-feature-range-notation`, the five
`*-no-vendor-prefix` rules (standard over prefixed), `plugin/use-baseline`.
stylelint `declaration-block-no-redundant-longhand-properties` stays only for
shorthands newer than their longhands (`inset`, `inset-block`, `inset-inline`,
`margin-block`, `margin-inline`, `padding-block`, `padding-inline`, `gap`,
`place-items`, `place-content`, `place-self`, `overflow`), as `modern` with
`low` confidence; its other findings (`margin`, `list-style`, …) are dropped.

## Stacks

**C#** (`.cs`). Pins: nearest `*.csproj`, then the `Directory.Build.props` chain
(`TargetFramework(s)`, `LangVersion`, `AnalysisLevel`), then `global.json`. The
touched projects and their `ProjectReference` closure are copied twice, along
with the props, `nuget.config` and `.editorconfig` above them. A
`modern-tara.slnx` is generated and restored in each copy. The two copies then
run in parallel: `dotnet format style` with IDE0019 IDE0020 IDE0028 IDE0038
IDE0041 IDE0066 IDE0078 IDE0083 IDE0161 IDE0290 IDE0300-IDE0306 IDE0330
IDE0360 IDE0090 IDE0057 IDE0056, and `dotnet format analyzers` with CA1510
CA1511 CA1512 CA1513 CA1839 CA1854 CA1860 CA1863 CA1870 CA1877 CA2263
SYSLIB1045 SYSLIB1054. A section appended to the copy's root `.editorconfig`
sets these IDs to `suggestion` and picks the modern style options (for
example, file-scoped namespaces). A closer `.editorconfig` still wins. Messages
are in English because of `DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1`.
`newest` (.NET 11 / C# 15) comes from a constant table; with SDK 10, C# 15
checks are catalogue only and no C# needs-newer findings come from the tools.
The IDE rules report without any `.editorconfig` severity, except IDE0161
(needs `file_scoped`). `--no-restore` works on a project that was never
restored, but package types stay unresolved, so the restore runs in the copy
(about 2 s with a warm NuGet cache). A combined `dotnet format` run is not
used: its whitespace pass shifts the line numbers.

**Rust** (`.rs`). Pins: nearest `Cargo.toml`, with workspace inheritance
(`edition`, `rust-version`). The workspace and its path dependencies are
copied, but `rust-toolchain*` is left out, so rustup never installs anything.
The tool runs `cargo clippy --message-format=json --all-targets --keep-going -- -W clippy::pedantic -W <15 lints>`.
It reports only modern-idiom lints: the `-W` list, every `manual_*` lint,
`MODERN_EXTRA` in `src/stacks/rust.rs` (map_unwrap_or, io_other_error,
legacy_numeric_constants, question_mark, ptr_as_ptr, …), and any message that
mentions stabilization or MSRV. A second copy gets `rust-version` raised to the
toolchain version and edition 2024 (if older), and clippy msrv is removed.
`clippy::incompatible_msrv` in the base run is `needs-newer`, with
`rust-version` raised to the version the lint names. Findings that appear only
in the raised run are `needs-newer`, and `message` says how many there were.
If the raised copy fails to compile, `message` says that instead.
`collapsible_if` is collected in both runs but reported only when the raised
run alone shows it: that means let chains (edition 2024, Rust 1.88). Clippy
will not collapse an `if` whose block contains a comment. `CARGO_TARGET_DIR`
is the [cache](#cache) folder for `fnv(repo|rust|workspace)`, with
`CARGO_INCREMENTAL=0`. Local-crate artifacts older than the run are pruned,
because each run's copy path makes a new hash. `CC` and `CXX` are removed from
the child environment on an `*-msvc` host.

**Python** (`.py .pyi`). Pins: nearest `pyproject.toml` `requires-python`,
else `.python-version`, else unknown (assumed 3.15). The tool runs
`ruff check --no-cache --isolated --select UP,FURB,PTH,SIM --target-version pyXY`
with no `--preview`, then the same at `py315`. Findings that only the newer
targets show are `needs-newer`. `to` is the first target that shows them,
found by running each version in between.

**JS/TS** (`.js .mjs .cjs .jsx .ts .mts .cts .tsx`). Pins: nearest
`package.json` (`engines.node`, `type`, `browserslist`) and `tsconfig.json`.
ESLint runs through `js/modern-tara.mjs` with the tool-owned flat config
`js/modern-tara.eslint.mjs`, which never loads a repository config. The rules:

- unicorn (`prefer-module` is reduced to one finding per file: its first use on
  a changed line, with the number of uses): prefer-at, prefer-structured-clone,
  prefer-node-protocol, prefer-string-replace-all, prefer-array-find,
  prefer-array-flat-map, prefer-array-some, prefer-includes,
  prefer-number-properties, prefer-global-this, prefer-module,
  prefer-top-level-await, prefer-abort-signal-timeout, prefer-url-can-parse,
  prefer-set-methods, prefer-iterator-helpers, prefer-promise-with-resolvers,
  prefer-import-meta-properties, prefer-string-starts-ends-with, prefer-spread,
  prefer-modern-dom-apis, prefer-modern-math-apis, prefer-object-from-entries,
  prefer-optional-catch-binding, no-for-each, prefer-group-by,
  prefer-array-from-async, prefer-regexp-escape.
- typescript-eslint: prefer-for-of. With a tsconfig, `projectService` also
  enables prefer-optional-chain, prefer-nullish-coalescing, prefer-includes and
  prefer-string-starts-ends-with. TypeScript reads the repository's
  `node_modules` through a read-only `ts.sys` overlay.
- core: prefer-object-has-own, no-var, prefer-template, object-shorthand.

`RULES` in `src/stacks/js.rs` maps each rule to the first Node version that
supports it. Above the lower bound of `engines.node` the finding is
`needs-newer` (`to: "26"`). TS syntax rules stay `modern`, because tsc
compiles them down.

eslint-plugin-es-x adds a `baseline` check for JS files. It covers ES2022+
features only, from the per-feature table `ES_X` in `src/stacks/js.rs`, which
gives each es-x rule and the first Node release that ships the feature
unflagged. Examples: `Object.hasOwn` 16.9, `findLast` 18.0, the `v` flag 20.0,
Set methods and iterator helpers 22.0, `RegExp.escape` 24.0. A rule is enabled
only when the `engines.node` minimum (major.minor) is below that release.
Source status (2026-11-22): `Float16Array` and `RegExp.escape` at 24.0 are
confirmed by the Node 24.0.0 release notes; the other minimums follow the V8
version each Node line ships and are not yet confirmed from a primary source.
Treat `baseline` findings as advisory. Features that unicorn also suggests
carry the same minimum in both tables, and a test checks this. The check is
skipped when `engines.node` is missing, and for TS files (tsc lowers the
syntax).

**CSS** (`.css`; scss is not scanned). stylelint 17 with
`js/modern-tara.stylelint.mjs`. These rules give `modern`:
color-function-notation (modern), media-feature-range-notation (context) and
the five `*-no-vendor-prefix` rules; declaration-block-no-redundant-longhand-properties
gives `modern` + `low` for the newer shorthands only. stylelint 17 has no core rule for logical
properties, so @eslint/css `css/prefer-logical-properties` covers them (kind
`modern`). `plugin/use-baseline` gives `baseline`, with `available` taken from
browserslist (`baseline widely|newly|<year>`), else `widely`.

**PowerShell** (`.ps1 .psm1`). Runs wherever `pwsh` (PowerShell 7) is found,
Linux included; without it the stack is `skipped`. Host: `#Requires -Version 6+`
/ `-PSEdition Core` means pwsh 7 and `-PSEdition Desktop` means 5.1. Otherwise
a launcher in the repo (.cmd .bat .ps1 .xml .json … including UTF-16 task
XML) that names the script within 3 lines of `pwsh` or `powershell.exe`
decides; with no launcher the host is unknown. `ps/run-pssa.ps1` runs through
`pwsh -NoProfile -NonInteractive -File`, never a command string built from
paths. Rules: PSAvoidUsingWMICmdlet gives `modern`; PSUseCompatibleSyntax
gives `baseline`, with TargetVersions 7.0, 5.1, or both for an unknown host. A
5.1 host adds a needs-newer note: PowerShell 7.7 is the newest stable.

**ast-grep catalogue** (`rules/*.yml`, `sgconfig.yml`; `metadata.gate/min`
decide needs-newer, `metadata.confidence` the confidence): C# `new HttpClient()`
in a method, `DateTime.UtcNow/Now` → `TimeProvider`, `new Random()` →
`Random.Shared` (all low); Rust `strip_prefix(..).unwrap_or(s | &s)` →
`trim_prefix` (1.100), `static X: OnceLock<T>` → `LazyLock` (1.80),
`Duration::from_secs(x * 60 | x * 3600)` → `from_mins`/`from_hours` (1.91;
clippy's duration_suboptimal_units only fires on constants) (all high); Python
`asyncio.gather` → `TaskGroup` (3.11, low); JS/TS axios / node-fetch → global
`fetch` (low). `JSON.parse(JSON.stringify())` has no rule here because unicorn
prefer-structured-clone already reports it.

## Tool home

modern-tara reads `js/`, `rules/`, `ps/`, `sgconfig.yml` and `tools.toml` from
its home folder: `MODERN_TARA_HOME` if set, else the nearest of the
executable's folder and its four parents that has `sgconfig.yml` and `js/`
(the unpacked release zip, or the repository for `target/<profile>/`), else
the executable's folder.

External tools are looked up on `PATH`, then in the usual install folders:
`~/.local/bin` (ruff, ast-grep), `~/.cargo/bin` (cargo, rustc, ast-grep), and
on Windows `%ProgramFiles%` (git, dotnet, node, pwsh), elsewhere
`~/.dotnet`, `/usr/share/dotnet`, `/opt/microsoft/powershell/7`, `/usr/bin`.

## Cache

The Rust stack's target folder is `<base>/<fnv64 hash>`. `<base>` is
`MODERN_TARA_CACHE` if set, else `<OS cache folder>/modern-tara/cache`:
`%LOCALAPPDATA%` on Windows, `$XDG_CACHE_HOME` or `~/.cache` on Linux,
`~/Library/Caches` on macOS. It reaches about 820 MB for a Tauri app. To
evict it, delete the folder (or the whole `cache` folder); the next run is
then a cold run.

## Pinned tools

`tools.toml` lists the exact version of each external tool: ruff, ast-grep,
PSScriptAnalyzer (`[sha256]` holds the SHA-256 of every download), and the npm set
(eslint, eslint-plugin-unicorn, eslint-plugin-es-x, typescript-eslint,
typescript, @eslint/css, stylelint, stylelint-plugin-use-baseline), which must
equal `js/package.json` (a test checks this). `modern-tara --version` prints a
`warning:` line for each pinned tool whose installed version differs or is
missing; the exit code stays 0. Rust, .NET, Node, git and pwsh come from the
machine.

Install or repair the set for the current user (both scripts read
`tools.toml` and leave a tool alone when it is already at its version):

- Windows: `scripts/install-tools.ps1` (PowerShell 7.4+): the ruff and
  ast-grep release archives for the platform from GitHub, SHA-256 checked,
  executables into `~/.local/bin` (`-BinDir`); PSScriptAnalyzer through
  `Save-PSResource -AsNupkg` into a temp folder, SHA-256 checked, then
  `Install-PSResource -Scope CurrentUser` from that folder; `npm ci
  --ignore-scripts` in `js/`. `-Only` runs some of these parts.
- Linux: `scripts/install-tools.sh`: the same, with PSScriptAnalyzer only when
  `pwsh` exists (through `install-tools.ps1 -Only PSScriptAnalyzer`).
  `MODERN_TARA_BIN` changes the executable folder.

Only x86_64 Windows and Linux have hashes in `tools.toml`; elsewhere the
scripts stop and name the missing `[sha256]` entry.

To move a pin: edit `tools.toml` (and, for npm, the exact versions in
`js/package.json`, then `npm install` in `js/`), check the rule names (the old
`no-array-for-each` is `no-for-each` in unicorn 80; stylelint-plugin-use-baseline
1.4.8 does not support stylelint 18), and raise the `NEWEST_*` constants in
`src/pins.rs` when a new stable ships.

## GitHub Action

```yaml
- uses: actions/checkout@<sha> # fetch-depth 0 saves the action an unshallow fetch
  with:
    fetch-depth: 0
- uses: bingoweb/modern-tara@v0.2.0
  with:
    args: --stacks rust,python
```

Inputs: `version` (a release tag; default the tag the action is used at),
`range` (default: for a pull request `merge-base(base, head)..head`, else
`github.event.before..github.sha`, or `sha~1..sha` for a new branch), `args`
(more `scan` arguments, split on spaces). A push whose `before` is gone
(force push) first fetches that commit; if it is still missing, or `before`
is all zeros, the range is `sha~1..sha`, and a root commit is scanned with
`--all-lines`.

The action stops on runners other than Windows or Linux x64. It sets up
Node 26 with `actions/setup-node`, which **stays on PATH for the job's later
steps**. It downloads `modern-tara-<target>.zip` for the runner, checks its
build provenance with `gh attestation verify <zip> --repo bingoweb/modern-tara`
and then its SHA-256 against the release's `SHA256SUMS`, runs the install
script, then `modern-tara scan --repo . --range <range> --gate` and writes the
stderr summary to the job summary. Output `report` is the JSON report's path.
Rust, .NET and pwsh come from the runner.

## Timings

Release build on Windows 11, stacks in parallel threads. "Cold" is the first
run, "warm" the second.

| repository / change | stacks | files | cold | warm |
|---|---|---|---|---|
| a .NET MCP server, a commit range | csharp | 6 | 31.5 s | 32.2 s |
| a small Rust crate, `--files src/*.rs --all-lines` (fresh cache) | rust | 8 | 1.8 s | 1.5 s |
| a Tauri app, last 5 `.rs` commits | rust | 5 | 129.8 s | 12.4 s |
| a Python + JS app, 4 commits | python, js | 16 + 1 | 2.7 s | 2.8 s |
| a Python + PowerShell tool, 10 commits | python, powershell | 2 + 2 | 11.9 s | 7.4 s |
| a JS course repository, 10 commits (`--git-safe`) | js | 39 | 6.0 s | 5.9 s |
| the Tauri app, last 10 TS commits | js (TS), css | 25 + 3 | 8.9 s | 7.9 s |

C# warm is not faster: both `dotnet format` passes reload the workspace on
every run. When the cold Tauri compile ran at the same time, the C# scan took
52–58 s. The worst case per stack is the timeout times the sequential
children:

- rust: 2 clippy runs × 900 s
- csharp: 300 s restore + 600 s format (the style and analyzer copies run in
  parallel)
- js: 300 s
- css: 120 s
- powershell: 180 s
- python: up to 9 ruff runs × 120 s (each normally under 0.2 s)

The ast-grep pass adds up to 60 s on every stack.

## Building and testing

`cargo build --release`; `cargo test` runs unit tests and fixture repositories
built from `tests/fixtures/` (stacks whose tool is missing are skipped);
`cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` are what
CI runs on Windows and Linux. A release is a `v*` tag matching `Cargo.toml`'s
version: `.github/workflows/release.yml` builds `x86_64-pc-windows-msvc` and
`x86_64-unknown-linux-gnu`, uploads `modern-tara-<target>.zip` (executable,
`js/` without `node_modules`, `rules/`, `ps/`, `scripts/`, `sgconfig.yml`,
`tools.toml`, `LICENSE`) and `SHA256SUMS`. The build jobs have read-only
permissions and hand the zips over as a workflow artifact; a separate `attest`
job checks them against their SHA-256 lines, attests their build provenance
and publishes the release.
