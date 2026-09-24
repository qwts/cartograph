import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { driftErrors, helpPages, syncWiki } from './check-help-mirror.mjs';

const script = path.join(path.dirname(new URL(import.meta.url).pathname), 'check-help-mirror.mjs');

function fixture(files) {
  const root = mkdtempSync(path.join(tmpdir(), 'help-mirror-'));
  const help = path.join(root, 'docs', 'help');
  mkdirSync(help, { recursive: true });
  for (const [name, body] of Object.entries(files)) writeFileSync(path.join(help, name), body);
  const wiki = path.join(root, 'wiki');
  mkdirSync(wiki);
  return { root, help, wiki };
}

test('--validate needs no wiki and rejects invalid slugs and empty topics (#470)', () => {
  const good = fixture({ 'concepts.md': '# Concepts\n', 'flow-inspector.md': '# Flows\n' });
  const ok = spawnSync(process.execPath, [script, '--validate'], { cwd: good.root, encoding: 'utf8' });
  assert.equal(ok.status, 0, ok.stderr);

  const bad = fixture({ 'Bad Name.md': '# x\n', 'empty.md': '  \n' });
  const result = spawnSync(process.execPath, [script, '--validate'], { cwd: bad.root, encoding: 'utf8' });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Bad Name\.md: slug must be lowercase kebab-case/u);
  assert.match(result.stderr, /empty\.md is empty/u);
});

test('--sync mirrors every topic, prunes orphaned Help pages, and leaves other wiki pages alone', () => {
  const { help, wiki } = fixture({ 'atlas.md': '# Atlas\n', 'gaps.md': '# Gaps\n' });
  writeFileSync(path.join(wiki, 'Help-removed.md'), 'stale');
  writeFileSync(path.join(wiki, 'Home.md'), 'home');
  const { pages, errors } = helpPages(help);
  assert.deepEqual(errors, []);
  assert.ok(driftErrors(pages, wiki).length > 0);

  const changes = syncWiki(pages, wiki);
  assert.deepEqual(changes, ['synced Help-atlas.md', 'synced Help-gaps.md', 'removed Help-removed.md']);
  assert.deepEqual(readdirSync(wiki).sort(), ['Help-atlas.md', 'Help-gaps.md', 'Home.md']);
  assert.match(readFileSync(path.join(wiki, 'Help-atlas.md'), 'utf8'), /# Atlas\n$/u);
  assert.deepEqual(driftErrors(pages, wiki), []);
  assert.deepEqual(syncWiki(pages, wiki), [], 'a second sync is a no-op');
});
