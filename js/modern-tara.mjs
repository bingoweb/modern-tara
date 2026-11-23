// modern-tara's Node driver: lints the temp copy with ESLint or stylelint
// through their APIs and prints one JSON object. Reads only the job file,
// the copy and (for type information) the repository's node_modules.
//
//   node modern-tara.mjs <job.json>

import fs from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const job = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const notes = [];
// Touched files the tool could not lint: { file, reason }.
const unscanned = [];

// The rule ids a config turns on (anything but 'off').
function enabledRules(config, into) {
  for (const entry of config) {
    for (const [id, level] of Object.entries(entry.rules ?? {})) {
      if ((Array.isArray(level) ? level[0] : level) !== 'off') into.add(id);
    }
  }
  return into;
}

const version = (pkg) => {
  try {
    // Read directly: some packages do not export ./package.json.
    return JSON.parse(fs.readFileSync(new URL(`./node_modules/${pkg}/package.json`, import.meta.url), 'utf8')).version;
  } catch {
    return '?';
  }
};

const norm = (p) => p.replaceAll('\\', '/');

function relative(root, filePath) {
  const r = norm(root).replace(/\/+$/, '');
  const f = norm(filePath);
  return f.toLowerCase().startsWith(`${r.toLowerCase()}/`) ? f.slice(r.length + 1) : f;
}

// TypeScript resolves packages by walking up to node_modules. The copy has
// none, so lookups under <copy>/**/node_modules read the repository's instead.
function overlayNodeModules(ts, root, repo) {
  if (!ts?.sys || !repo) return;
  const rootN = norm(root).replace(/\/+$/, '');
  const rootL = `${rootN.toLowerCase()}/`;
  const repoN = norm(repo).replace(/\/+$/, '');
  const map = (p) => {
    if (typeof p !== 'string') return p;
    const n = norm(p);
    if (!n.toLowerCase().startsWith(rootL)) return p;
    const rest = n.slice(rootN.length);
    return /\/node_modules(\/|$)/.test(rest) ? repoN + rest : p;
  };
  for (const fn of ['fileExists', 'readFile', 'directoryExists', 'getDirectories', 'realpath', 'getFileSize', 'getModifiedTime', 'readDirectory']) {
    const original = ts.sys[fn];
    if (typeof original === 'function') ts.sys[fn] = (p, ...rest) => original.call(ts.sys, map(p), ...rest);
  }
}

const STRICT_NULL = /strictNullChecks/;

async function runEslint() {
  const ts = require('typescript');
  overlayNodeModules(ts, job.root, job.repo);
  const { ESLint } = await import('eslint');
  const { buildConfig, ruleIds } = await import('./modern-tara.eslint.mjs');
  const wanted = ruleIds();
  const results = [];
  const kinds = {};
  const warnings = [];
  let files = job.files;
  let strictNullNote = false;
  const rules = new Set();
  const seen = new Set();

  for (let attempt = 0; attempt < 2 && files.length > 0; attempt++) {
    const config = buildConfig(job.root, files);
    enabledRules(config, rules);
    const eslint = new ESLint({
      cwd: job.root,
      overrideConfigFile: true,
      overrideConfig: config,
      cache: false,
      ignore: false,
      errorOnUnmatchedPattern: false,
      warnIgnored: false,
    });
    const linted = await eslint.lintFiles(files.map((f) => f.path));
    const retry = [];
    for (const r of linted) {
      const file = relative(job.root, r.filePath);
      const entry = files.find((f) => f.path.toLowerCase() === file.toLowerCase());
      if (entry) seen.add(entry.path);
      const fatal = r.messages.find((m) => m.fatal);
      const unparsed = (reason) => {
        notes.push(`${file}: ${reason}`);
        unscanned.push({ file, reason });
      };
      if (fatal) {
        if (attempt === 0 && entry && !entry.ts && entry.sourceType === 'script') {
          // Said to be a classic script but it is not one: lint it as the ES module it is.
          retry.push({ ...entry, sourceType: 'module', fromScript: true });
        } else if (attempt === 0 && entry) {
          // Retry once: the other module kind for JS, no type info for TS.
          if (entry.ts && entry.typeAware) retry.push({ ...entry, typeAware: false });
          else if (!entry.ts) retry.push({ ...entry, sourceType: entry.sourceType === 'module' ? 'commonjs' : 'module' });
          else unparsed(`not parsed: ${fatal.message}`);
        } else if (entry?.fromScript) {
          unparsed(`not parsed as a classic script or as an ES module: ${fatal.message}`);
        } else {
          unparsed(`not parsed: ${fatal.message}`);
        }
        continue;
      }
      if (entry) {
        kinds[entry.path] = entry.sourceType;
        if (entry.fromScript) warnings.push(`${entry.path} is in classic-scripts but parses only as an ES module`);
      }
      for (const m of r.messages) {
        if (!m.ruleId || !(wanted.has(m.ruleId) || m.ruleId.startsWith('es-x/'))) continue;
        if (STRICT_NULL.test(m.message) && m.line === 1 && m.ruleId.includes('nullish')) {
          strictNullNote = true;
          continue;
        }
        // `fixable`: ESLint offers an autofix (a suggestion alone does not count).
        results.push({ file, line: m.line, column: m.column, rule: m.ruleId, message: m.message, fixable: Boolean(m.fix) });
      }
    }
    if (retry.length > 0 && attempt === 0) {
      const typedOff = retry.filter((f) => f.ts).length;
      if (typedOff > 0) notes.push(`${typedOff} TypeScript file(s) were not in a tsconfig project: linted without type information.`);
    }
    files = retry;
  }
  for (const f of job.files) {
    if (!seen.has(f.path)) unscanned.push({ file: f.path, reason: 'ESLint returned no result for it' });
  }
  if (strictNullNote) notes.push('prefer-nullish-coalescing needs strictNullChecks; it was skipped where the tsconfig lacks it.');
  const tool = `eslint ${version('eslint')} (unicorn ${version('eslint-plugin-unicorn')}, es-x ${version('eslint-plugin-es-x')}, typescript-eslint ${version('typescript-eslint')}, typescript ${version('typescript')})`;
  return { tool, results, notes, warnings, kinds, unscanned, rules: [...rules].sort() };
}

async function runStylelint() {
  const stylelint = (await import('stylelint')).default;
  const baselineModule = await import('stylelint-plugin-use-baseline');
  const useBaseline = baselineModule.default ?? baselineModule;
  const { default: config } = await import('./modern-tara.stylelint.mjs');
  const results = [];
  const rules = new Set();
  for (const f of job.files) {
    const abs = `${norm(job.root)}/${f.path}`;
    let code;
    try {
      code = fs.readFileSync(abs, 'utf8');
    } catch (e) {
      unscanned.push({ file: f.path, reason: `not readable: ${e.message}` });
      continue;
    }
    const fileConfig = config(useBaseline, f.available);
    for (const id of Object.keys(fileConfig.rules)) rules.add(id);
    const res = await stylelint.lint({
      code,
      codeFilename: abs,
      config: fileConfig,
      cache: false,
      quietDeprecationWarnings: true,
      // A stylelint-disable comment never hides a finding (only modern-tara.toml waives).
      ignoreDisables: true,
    });
    for (const r of res.results) {
      for (const w of r.warnings) {
        if (w.rule === 'CssSyntaxError') {
          notes.push(`${f.path}: not parsed: ${w.text}`);
          unscanned.push({ file: f.path, reason: `not parsed: ${w.text}` });
          continue;
        }
        results.push({ file: f.path, line: w.line, column: w.column, rule: w.rule, message: w.text });
      }
      for (const w of r.invalidOptionWarnings ?? []) notes.push(`stylelint option: ${w.text}`);
    }
  }
  // @eslint/css: physical properties that have a logical equivalent, only
  // those in job.logicalProperties (the rule's own options allow every
  // other property and every unit); values are classified by modern-tara.
  const { ESLint } = await import('eslint');
  const css = (await import('@eslint/css')).default;
  const logical = 'css/prefer-logical-properties';
  const schema = css.rules['prefer-logical-properties'].meta.schema[0].properties;
  const keep = new Set(job.logicalProperties ?? []);
  const options = {
    allowProperties: schema.allowProperties.items.enum.filter((p) => !keep.has(p)),
    allowUnits: [...schema.allowUnits.items.enum],
  };
  rules.add(logical);
  const eslint = new ESLint({
    cwd: job.root,
    overrideConfigFile: true,
    overrideConfig: [
      {
        files: ['**/*.css'],
        language: 'css/css',
        languageOptions: { tolerant: true },
        plugins: { css },
        linterOptions: { noInlineConfig: true, reportUnusedDisableDirectives: 'off' },
        rules: { [logical]: ['warn', options] },
      },
    ],
    cache: false,
    ignore: false,
    errorOnUnmatchedPattern: false,
    warnIgnored: false,
  });
  for (const r of await eslint.lintFiles(job.files.map((f) => f.path))) {
    const file = relative(job.root, r.filePath);
    for (const m of r.messages) {
      if (m.fatal) unscanned.push({ file, reason: `@eslint/css did not parse it: ${m.message}` });
      else if (m.ruleId === logical) results.push({ file, line: m.line, column: m.column, rule: m.ruleId, message: m.message });
    }
  }
  const tool = `stylelint ${version('stylelint')} (stylelint-plugin-use-baseline ${version('stylelint-plugin-use-baseline')}), eslint ${version('eslint')} (@eslint/css ${version('@eslint/css')})`;
  return { tool, results, notes, unscanned, rules: [...rules].sort() };
}

const out = job.mode === 'stylelint' ? await runStylelint() : await runEslint();
process.stdout.write(JSON.stringify(out));
