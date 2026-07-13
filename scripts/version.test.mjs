import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { checkVersions, syncVersions } from './version.mjs';

const json = (value) => `${JSON.stringify(value, null, 2)}\n`;

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), 'cartograph-version-'));
  mkdirSync(path.join(root, 'ui'), { recursive: true });
  mkdirSync(path.join(root, 'src-tauri'), { recursive: true });
  mkdirSync(path.join(root, 'crates/example'), { recursive: true });
  writeFileSync(path.join(root, 'package.json'), json({ name: 'cartograph', version: '0.2.3' }));
  writeFileSync(
    path.join(root, 'package-lock.json'),
    json({ name: 'cartograph', version: '0.2.3', packages: { '': { version: '0.2.3' } } }),
  );
  writeFileSync(path.join(root, 'ui/package.json'), json({ name: 'ui', version: '0.2.3' }));
  writeFileSync(
    path.join(root, 'ui/package-lock.json'),
    json({ name: 'ui', version: '0.2.3', packages: { '': { version: '0.2.3' } } }),
  );
  writeFileSync(
    path.join(root, 'Cargo.toml'),
    '[workspace]\nmembers = ["crates/*", "src-tauri"]\n\n[workspace.package]\nversion = "0.2.3"\nedition = "2024"\n',
  );
  writeFileSync(
    path.join(root, 'src-tauri/Cargo.toml'),
    '[package]\nname = "cartograph-app"\nversion.workspace = true\n',
  );
  writeFileSync(
    path.join(root, 'crates/example/Cargo.toml'),
    '[package]\nname = "example"\nversion.workspace = true\n',
  );
  writeFileSync(
    path.join(root, 'Cargo.lock'),
    'version = 4\n\n[[package]]\nname = "cartograph-app"\nversion = "0.2.3"\n\n[[package]]\nname = "example"\nversion = "0.2.3"\n',
  );
  writeFileSync(
    path.join(root, 'src-tauri/tauri.conf.json'),
    json({ productName: 'Cartograph', version: '0.2.3' }),
  );
  return root;
}

test('checkVersions accepts a synchronized workspace', (t) => {
  const root = fixture();
  t.after(() => rmSync(root, { recursive: true, force: true }));
  assert.equal(checkVersions(root), '0.2.3');
});

test('checkVersions reports every drifted source', (t) => {
  const root = fixture();
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const tauriPath = path.join(root, 'src-tauri/tauri.conf.json');
  writeFileSync(tauriPath, json({ productName: 'Cartograph', version: '9.9.9' }));
  assert.throws(
    () => checkVersions(root),
    /src-tauri\/tauri\.conf\.json: 9\.9\.9 \(expected 0\.2\.3\)/u,
  );
});

test('syncVersions updates every mirror and workspace lock entry', (t) => {
  const root = fixture();
  t.after(() => rmSync(root, { recursive: true, force: true }));
  assert.equal(syncVersions(root, '0.3.0'), '0.3.0');
  assert.equal(checkVersions(root), '0.3.0');
  const cargoLock = readFileSync(path.join(root, 'Cargo.lock'), 'utf8');
  assert.equal([...cargoLock.matchAll(/^version = "0\.3\.0"$/gmu)].length, 2);
});

test('syncVersions rejects malformed SemVer before writing', (t) => {
  const root = fixture();
  t.after(() => rmSync(root, { recursive: true, force: true }));
  assert.throws(() => syncVersions(root, '01.2.3'), /Invalid semantic version/u);
  assert.equal(checkVersions(root), '0.2.3');
});
