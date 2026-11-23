// Flat ESLint config owned by modern-tara: modernizer rules only, built per
// run for the files of the temp copy. Never merged with a repository config.

import esx from 'eslint-plugin-es-x';
import unicorn from 'eslint-plugin-unicorn';
import tseslint from 'typescript-eslint';

// es-x rules come per file from modern-tara (`esx`): only features the
// engines.node minimum does not ship yet; reported as kind "baseline".

// eslint-plugin-unicorn 80 names the old no-array-for-each rule `no-for-each`.
export const UNICORN = [
  'prefer-at',
  'prefer-structured-clone',
  'prefer-node-protocol',
  'prefer-string-replace-all',
  'prefer-array-find',
  'prefer-array-flat-map',
  'prefer-array-some',
  'prefer-includes',
  'prefer-number-properties',
  'prefer-global-this',
  'prefer-module',
  'prefer-top-level-await',
  'prefer-abort-signal-timeout',
  'prefer-url-can-parse',
  'prefer-set-methods',
  'prefer-iterator-helpers',
  'prefer-promise-with-resolvers',
  'prefer-import-meta-properties',
  'prefer-string-starts-ends-with',
  'prefer-spread',
  'prefer-modern-dom-apis',
  'prefer-modern-math-apis',
  'prefer-object-from-entries',
  'prefer-optional-catch-binding',
  'no-for-each',
  'prefer-group-by',
  'prefer-array-from-async',
  'prefer-regexp-escape',
  // Shorter code: no-array-sort and no-array-reverse (toSorted/toReversed, ES2023) have no autofix.
  'no-for-loop',
  'no-useless-spread',
  'prefer-negative-index',
  'prefer-string-slice',
  'no-array-sort',
  'no-array-reverse',
];

// typescript-eslint rules that need type information (projectService).
export const TS_TYPED = ['prefer-optional-chain', 'prefer-nullish-coalescing', 'prefer-includes', 'prefer-string-starts-ends-with'];
export const TS_UNTYPED = ['prefer-for-of'];
// Core rules that move code to an ES2015+ construct. prefer-const is left out:
// let and const are the same generation, so it is style, not age.
export const CORE = ['prefer-object-has-own', 'no-var', 'prefer-template', 'object-shorthand'];

export function ruleIds() {
  return new Set([
    ...UNICORN.map((r) => `unicorn/${r}`),
    ...TS_TYPED.map((r) => `@typescript-eslint/${r}`),
    ...TS_UNTYPED.map((r) => `@typescript-eslint/${r}`),
    ...CORE,
  ]);
}

const norm = (p) => p.replaceAll('\\', '/').toLowerCase();

// File patterns are functions on the absolute path, so names with glob
// characters (`[id].tsx`, `(group)`) match exactly.
function matcher(root, paths) {
  const wanted = new Set(paths.map((p) => norm(`${root}/${p}`)));
  return (filePath) => wanted.has(norm(filePath));
}

// A classic script (`sourceType: 'script'`, browser code in a package that
// is not a module) cannot use these: they all ask for ES modules.
export const MODULE_ONLY = ['prefer-module', 'prefer-top-level-await', 'prefer-import-meta-properties'];

function rulesFor({ ts, typeAware, esx: esxRules = [], sourceType }) {
  const rules = {};
  for (const r of esxRules) rules[r] = 'warn';
  for (const r of UNICORN) rules[`unicorn/${r}`] = 'warn';
  for (const r of CORE) rules[r] = 'warn';
  if (!ts && sourceType === 'script') {
    for (const r of MODULE_ONLY) rules[`unicorn/${r}`] = 'off';
  }
  if (ts) {
    for (const r of TS_UNTYPED) rules[`@typescript-eslint/${r}`] = 'warn';
    if (typeAware) {
      for (const r of TS_TYPED) rules[`@typescript-eslint/${r}`] = 'warn';
      // The typed versions know the receiver type; avoid double reports.
      rules['unicorn/prefer-includes'] = 'off';
      rules['unicorn/prefer-string-starts-ends-with'] = 'off';
    }
  }
  return rules;
}

/** @param files {{path: string, sourceType: string, typeAware: boolean, ts: boolean}[]} */
export function buildConfig(root, files) {
  const groups = new Map();
  for (const f of files) {
    const key = `${f.ts}|${f.sourceType}|${f.typeAware}|${(f.esx ?? []).join(',')}`;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(f);
  }
  const config = [
    {
      // Inline eslint-disable comments never hide a finding: only modern-tara.toml waives.
      linterOptions: { noInlineConfig: true, reportUnusedDisableDirectives: 'off', reportUnusedInlineConfigs: 'off' },
    },
  ];
  for (const group of groups.values()) {
    const { ts, sourceType, typeAware, esx: esxList } = group[0];
    const languageOptions = ts
      ? {
          parser: tseslint.parser,
          sourceType: 'module',
          parserOptions: typeAware ? { projectService: true, tsconfigRootDir: root } : {},
        }
      : {
          ecmaVersion: 'latest',
          sourceType,
          parserOptions: { ecmaFeatures: { jsx: true } },
        };
    config.push({
      files: [matcher(root, group.map((f) => f.path))],
      languageOptions,
      plugins: ts ? { unicorn, '@typescript-eslint': tseslint.plugin } : { unicorn, 'es-x': esx },
      rules: rulesFor({ ts, typeAware, esx: esxList, sourceType }),
    });
  }
  return config;
}
