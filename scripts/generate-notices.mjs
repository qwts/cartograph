#!/usr/bin/env node
// Generate THIRD-PARTY-NOTICES.md (#222): the attribution + license texts for
// every third-party dependency Cartograph ships — Rust crates (via cargo-about)
// and the production npm packages in ui/.
//
// Output is DETERMINISTIC (no timestamps, sorted) so CI can regenerate and diff
// it against the committed copy to catch drift. Also emits a code-split TS
// module the app imports for its in-app "Open-source licenses" view.
//
// Usage:  node scripts/generate-notices.mjs [--check]
//   (no flag)  write the files
//   --check    regenerate in memory and fail if the committed files are stale
//
// Requires: cargo-about on PATH, and `npm ci` already run in ui/ (the JS
// collector derives the inventory from ui/package-lock.json but reads each
// package's license text from the installed copy).

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { dirname as posixDirname, join as posixJoin } from 'node:path/posix';
import { fileURLToPath } from 'node:url';

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const NOTICES_PATH = join(ROOT, 'THIRD-PARTY-NOTICES.md');
const TS_PATH = join(ROOT, 'ui', 'src', 'generated', 'thirdPartyNotices.ts');
const LOCK_PATH = join(ROOT, 'ui', 'package-lock.json');
const check = process.argv.includes('--check');

function run(cmd, args, cwd) {
  return execFileSync(cmd, args, {
    cwd,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'inherit'],
  });
}

// --- Rust: cargo-about renders about.hbs against the locked crate graph. ---
// `--offline --locked` keeps output reproducible (no clearlydefined.io / lock
// drift) so the CI drift-check is stable. cargo-deny is the license *gate*;
// this step only renders notices, so our own (PolyForm-licensed) workspace
// crates are simply skipped by cargo-about with a warning rather than failing.
function rustNotices() {
  const out = run('cargo', ['about', 'generate', 'about.hbs', '--offline', '--locked'], ROOT);
  return out.trimEnd();
}

// --- JavaScript: derive the production inventory from ui/package-lock.json. ---
// The lockfile — not `npm ls` — is the source of truth: it is committed, so
// the inventory cannot drift with the ambient npm major or a dirty
// node_modules. The production closure is walked from the root manifest's
// non-dev entry points (dependencies + optionalDependencies + installed
// peers); devDependencies are never traversed. Install locations come from
// lockfile keys, so no hoisting layout is assumed. Platform-gated optional
// packages stay in the inventory even when not installed on the running
// machine, and their installed copy never feeds the output (see
// readJsLicense), keeping regeneration byte-identical across OSes and npm
// majors.
export function collectJsPackages(lockJson) {
  const packages = lockJson.packages ?? {};
  const root = packages[''] ?? {};
  const rootNames = [
    ...Object.keys(root.dependencies ?? {}),
    ...Object.keys(root.optionalDependencies ?? {}),
    ...Object.keys(root.peerDependencies ?? {}),
  ];

  // Resolve `name` required by the package at `fromKey` the way npm does:
  // nearest node_modules ancestor first. Lockfile keys are ui/-relative
  // POSIX paths like "node_modules/a/node_modules/b" — always "/"-separated,
  // on every OS — so candidate keys must be built with path.posix, never the
  // platform-native join/dirname (which emit backslashes on Windows and
  // would miss every lookup).
  const resolveName = (fromKey, name) => {
    let dir = fromKey === '' ? '.' : fromKey;
    for (;;) {
      const candidate = posixJoin(dir, 'node_modules', name);
      if (candidate in packages) return candidate;
      if (dir === '.') return undefined;
      dir = posixDirname(dir);
    }
  };

  const closure = new Map(); // lockfile key -> {name, version, optional}
  const stack = rootNames.map((name) => resolveName('', name));
  while (stack.length > 0) {
    const key = stack.pop();
    if (key === undefined) continue;
    const entry = packages[key];
    if (!entry || entry.version == null || entry.dev === true) continue;
    if (closure.has(key)) continue;
    const name = key.split('node_modules/').pop();
    closure.set(key, { name, version: entry.version, optional: entry.optional === true, lockKey: key });
    const deps = {
      ...(entry.dependencies ?? {}),
      ...(entry.optionalDependencies ?? {}),
      ...(entry.peerDependencies ?? {}),
    };
    for (const depName of Object.keys(deps)) stack.push(resolveName(key, depName));
  }
  return [...closure.values()].sort((a, b) =>
    a.name === b.name ? a.version.localeCompare(b.version) : a.name.localeCompare(b.name),
  );
}

export function readJsLicense(pkg, baseDir = join(ROOT, 'ui')) {
  // Platform-gated optional packages are absent from node_modules on the OSes
  // they do not target, so their installed copy must never feed the output:
  // on the matching platform it exists (real license text), elsewhere it does
  // not ("See package"), and the same lockfile would regenerate differently
  // per OS. Optionals always get the stable annotated entry instead; only
  // packages every production install must have are read from disk.
  if (pkg.optional) {
    return { spdx: 'See package', text: '', platformGated: true };
  }
  // The lockfile key is the exact install path relative to ui/, so nested
  // duplicates resolve correctly without assuming hoisting.
  const dir = join(baseDir, pkg.lockKey);
  let spdx = 'See package';
  let text = '';
  const manifestPath = join(dir, 'package.json');
  if (existsSync(manifestPath)) {
    try {
      const m = JSON.parse(readFileSync(manifestPath, 'utf8'));
      if (typeof m.license === 'string') spdx = m.license;
      else if (m.license?.type) spdx = m.license.type;
      else if (Array.isArray(m.licenses)) spdx = m.licenses.map((l) => l.type).join(' OR ');
    } catch {
      /* keep default */
    }
    try {
      const file = readdirSync(dir).find((f) => /^(LICEN[CS]E|COPYING)/i.test(f));
      if (file) text = readFileSync(join(dir, file), 'utf8').trimEnd();
    } catch {
      /* no license file bundled */
    }
  }
  return { spdx, text, platformGated: false };
}

function jsNotices() {
  const pkgs = collectJsPackages(JSON.parse(readFileSync(LOCK_PATH, 'utf8')));
  const lines = [
    'Cartograph bundles the following npm packages in its web UI. Each is used',
    'under the terms of its declared license. This list is generated from the',
    'production dependency tree of `ui/package-lock.json`; do not edit by hand.',
    '',
  ];
  for (const pkg of pkgs) {
    const { spdx, text, platformGated } = readJsLicense(pkg);
    lines.push(`## ${pkg.name} ${pkg.version}`, '', `License: ${spdx}`, '');
    if (platformGated) {
      lines.push(
        'Platform-gated optional dependency: it is installed only on the OSes',
        'it targets, so its license text is not reproduced here. See the',
        'package itself for its license terms.',
        '',
      );
    } else if (text) {
      lines.push('```', text, '```', '');
    }
  }
  return lines.join('\n').trimEnd();
}

function build() {
  const body = [
    '# Third-Party Notices',
    '',
    'Cartograph itself is licensed under the PolyForm Noncommercial License 1.0.0',
    '(see `LICENSE.md`). It also incorporates third-party open-source software,',
    'each under its own license, reproduced in full below.',
    '',
    'This file is generated — run `node scripts/generate-notices.mjs` to refresh',
    'it. CI fails if it drifts from the dependency manifests.',
    '',
    '---',
    '',
    '# Rust crates',
    '',
    rustNotices(),
    '',
    '---',
    '',
    '# npm packages (web UI)',
    '',
    jsNotices(),
    '',
  ].join('\n');
  return body;
}

function tsModule(markdown) {
  return (
    '// GENERATED by scripts/generate-notices.mjs — do not edit.\n' +
    '// Full third-party attribution for the in-app "Open-source licenses" view.\n' +
    `export const THIRD_PARTY_NOTICES = ${JSON.stringify(markdown)};\n`
  );
}

// Only run the generator when invoked directly, so tests can import the
// collector (same pattern as scripts/version.mjs).
const SCRIPT_PATH = fileURLToPath(import.meta.url);
if (process.argv[1] && resolve(process.argv[1]) === SCRIPT_PATH) {
  const notices = build();
  const ts = tsModule(notices);

  if (check) {
    const staleNotices = !existsSync(NOTICES_PATH) || readFileSync(NOTICES_PATH, 'utf8') !== notices;
    const staleTs = !existsSync(TS_PATH) || readFileSync(TS_PATH, 'utf8') !== ts;
    if (staleNotices || staleTs) {
      console.error(
        'THIRD-PARTY-NOTICES is out of date. Run `node scripts/generate-notices.mjs` and commit the result.',
      );
      process.exit(1);
    }
    console.log('Third-party notices are up to date.');
  } else {
    writeFileSync(NOTICES_PATH, notices);
    writeFileSync(TS_PATH, ts);
    console.log(`Wrote ${NOTICES_PATH} and ${TS_PATH}.`);
  }
}
