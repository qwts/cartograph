import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { PIN, verifyInventory } from './run-local-investigation-acceptance.mjs';

test('local acceptance rejects missing, ambiguous or changed model identity before inference', () => {
  // AC-0191: a mutable model tag alone is not pinned-model acceptance evidence.
  const model = { name: PIN.model, digest: PIN.modelDigest, size: 5225388177 };
  assert.equal(verifyInventory({ models: [model] }), model);
  assert.equal(verifyInventory({ models: [{ ...model, digest: `sha256:${PIN.modelDigest}` }] }).name, PIN.model);
  for (const models of [[], [model, model], [{ ...model, name: 'different:tag' }],
    [{ ...model, digest: '0'.repeat(64) }], [{ name: PIN.model }]]) {
    assert.throws(() => verifyInventory({ models }));
  }
});

test('the remote model driver refuses an ordinary local invocation before setup', () => {
  // AC-0191: this does not invoke a model; it verifies the fail-closed entry guard.
  const result = spawnSync(process.execPath,
    [fileURLToPath(new URL('./run-local-investigation-acceptance.mjs', import.meta.url))], {
      env: { ...process.env, GITHUB_ACTIONS: 'false' }, encoding: 'utf8', timeout: 5000,
    });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Use the governed GitHub runner/u);
});
