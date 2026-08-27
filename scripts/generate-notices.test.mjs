import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import test from 'node:test';
import { join } from 'node:path';

import { collectJsPackages, readJsLicense } from './generate-notices.mjs';

// Synthetic lockfile exercising every inventory rule (#340): the production
// closure starts at the root manifest's dependencies/optionalDependencies/
// peerDependencies, never devDependencies; nested duplicates resolve through
// lockfile keys; platform-gated optionals stay in the inventory.
function lockFixture() {
  return {
    lockfileVersion: 3,
    packages: {
      '': {
        dependencies: { hoisted: '^1.0.0', peerly: '^5.0.0' },
        devDependencies: { 'dev-tool': '^9.0.0' },
      },
      'node_modules/hoisted': {
        version: '1.2.3',
        dependencies: { shared: '^3.0.0' },
        optionalDependencies: { 'platform-opt': '^4.0.0' },
      },
      'node_modules/app-prod': { version: '2.0.0' },
      // Reached transitively from the root via a nested duplicate.
      'node_modules/hoisted/node_modules/shared': { version: '3.0.0' },
      'node_modules/shared': { version: '3.5.0' },
      // Runtime optional dependency: stays in the inventory even though a
      // platform gate may leave it uninstalled.
      'node_modules/hoisted/node_modules/platform-opt': {
        version: '4.1.0',
        optional: true,
        os: ['darwin'],
        cpu: ['arm64'],
      },
      // Prod package whose peer is installed at the top level.
      'node_modules/peerly': {
        version: '5.0.0',
        peerDependencies: { 'peer-dep': '^6.0.0' },
      },
      'node_modules/peer-dep': { version: '6.0.0' },
      // Dev-only packages: must never appear regardless of edges.
      'node_modules/dev-tool': { version: '9.9.9' },
      'node_modules/dev-dep-of-dev': { version: '8.0.0' },
    },
  };
}

test('npm inventory comes from the lockfile production closure, not npm ls', () => {
  const pkgs = collectJsPackages(lockFixture());
  assert.deepEqual(
    pkgs.map((p) => [p.name, p.version, p.optional]),
    [
      ['hoisted', '1.2.3', false],
      ['peer-dep', '6.0.0', false],
      ['peerly', '5.0.0', false],
      ['platform-opt', '4.1.0', true],
      ['shared', '3.0.0', false],
    ],
  );
});

test('dev-only packages and devDependencies edges are never traversed', () => {
  const names = collectJsPackages(lockFixture()).map((p) => p.name);
  assert.ok(!names.includes('dev-tool'));
  assert.ok(!names.includes('dev-dep-of-dev'));
});

test('nested duplicates resolve to the version their dependent actually ships', () => {
  // hoisted (at node_modules/hoisted) requires shared@3.0.0 nested inside it,
  // not the hoisted shared@3.5.0 that other roots would see.
  const pkgs = collectJsPackages(lockFixture());
  assert.deepEqual(
    pkgs.filter((p) => p.name === 'shared').map((p) => [p.version, p.lockKey]),
    [['3.0.0', 'node_modules/hoisted/node_modules/shared']],
  );
});

test('runtime optionals stay in the inventory even when platform-gated', () => {
  const pkgs = collectJsPackages(lockFixture());
  const opt = pkgs.find((p) => p.name === 'platform-opt');
  assert.ok(opt);
  assert.equal(opt.optional, true);
});

test('lockfile-key resolution only uses POSIX separators', () => {
  // npm writes lockfile keys with "/" on every OS. Candidate keys must be
  // built the same way (path.posix), otherwise resolution silently misses on
  // Windows where path.join emits "\". Asserted via a deep nesting: the
  // nearest node_modules ancestor must win over shallower duplicates.
  const lock = lockFixture();
  lock.packages['node_modules/deep'] = { version: '7.0.0', dependencies: { shared: '^3.0.0' } };
  lock.packages[''].dependencies.deep = '^7.0.0';
  const shared = collectJsPackages(lock).filter((p) => p.name === 'shared');
  // `deep` has no nested copy, so it resolves the hoisted shared@3.5.0 while
  // `hoisted` still ships its own shared@3.0.0 — both keys, both POSIX.
  assert.ok(
    shared.every((p) => p.lockKey.split('\\').length === 1),
    `lockfile keys must contain no backslashes: ${shared.map((p) => p.lockKey)}`,
  );
  assert.deepEqual(
    shared.map((p) => p.lockKey),
    ['node_modules/hoisted/node_modules/shared', 'node_modules/shared'],
  );
});

test('optionals never read the installed copy; regular packages do', () => {
  const baseDir = mkdtempSync(join(tmpdir(), 'notices-'));
  try {
    // The optional package IS installed here (as on its matching OS): the
    // output must still be the stable annotated entry, not this copy's text.
    const optDir = join(baseDir, 'node_modules', 'platform-opt');
    mkdirSync(optDir, { recursive: true });
    writeFileSync(join(optDir, 'package.json'), JSON.stringify({ license: 'MIT' }));
    writeFileSync(join(optDir, 'LICENSE'), 'FAKE OPT LICENSE');
    const regDir = join(baseDir, 'node_modules', 'regular');
    mkdirSync(regDir, { recursive: true });
    writeFileSync(join(regDir, 'package.json'), JSON.stringify({ license: 'Apache-2.0' }));
    writeFileSync(join(regDir, 'LICENSE'), 'FAKE REG LICENSE');

    assert.deepEqual(readJsLicense({ lockKey: 'node_modules/platform-opt', optional: true }, baseDir), {
      spdx: 'See package',
      text: '',
      platformGated: true,
    });
    assert.deepEqual(readJsLicense({ lockKey: 'node_modules/regular', optional: false }, baseDir), {
      spdx: 'Apache-2.0',
      text: 'FAKE REG LICENSE',
      platformGated: false,
    });
  } finally {
    rmSync(baseDir, { recursive: true, force: true });
  }
});

test('the output is deterministically sorted by name then version', () => {
  const lock = lockFixture();
  // A second consumer of `shared` that has no nested copy resolves to the
  // hoisted 3.5.0, so the inventory holds shared@3.0.0 and shared@3.5.0.
  lock.packages['node_modules/fanout'] = { version: '1.0.0', dependencies: { shared: '^3.0.0' } };
  lock.packages[''].dependencies.fanout = '^1.0.0';
  const sharedVersions = collectJsPackages(lock)
    .filter((p) => p.name === 'shared')
    .map((p) => p.version);
  assert.deepEqual(sharedVersions, ['3.0.0', '3.5.0']);
});
