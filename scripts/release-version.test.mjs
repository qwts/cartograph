import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { pendingChangesets, tagForVersion } from './release-version.mjs';

test('tagForVersion emits an exact SemVer tag', () => {
  assert.equal(tagForVersion('0.12.3'), 'v0.12.3');
  assert.equal(tagForVersion('1.0.0-rc.1'), 'v1.0.0-rc.1');
  assert.throws(() => tagForVersion('1.2'), /Invalid semantic version/u);
});

test('pendingChangesets excludes metadata and sorts release intent', (t) => {
  const root = mkdtempSync(path.join(os.tmpdir(), 'cartograph-changesets-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(path.join(root, '.changeset'));
  for (const name of ['README.md', 'config.json', 'zebra-note.md', 'alpha-note.md']) {
    writeFileSync(path.join(root, '.changeset', name), '');
  }
  assert.deepEqual(pendingChangesets(root), ['alpha-note.md', 'zebra-note.md']);
});
