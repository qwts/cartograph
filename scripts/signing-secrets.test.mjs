import assert from 'node:assert/strict';
import test from 'node:test';

import { SIGNING_SECRET_NAMES, signingMode } from './signing-secrets.mjs';

test('an empty signing environment is visibly unsigned', () => {
  assert.deepEqual(signingMode({}), { mode: 'unsigned-dev', signed: false });
});

test('all five signing secrets enable the production path', () => {
  const env = Object.fromEntries(SIGNING_SECRET_NAMES.map((name) => [name, `${name}-value`]));
  assert.deepEqual(signingMode(env), { mode: 'signed', signed: true });
});

test('every partial signing set fails closed without exposing values', () => {
  for (const omitted of SIGNING_SECRET_NAMES) {
    const env = Object.fromEntries(
      SIGNING_SECRET_NAMES.filter((name) => name !== omitted).map((name) => [name, 'sensitive']),
    );

    assert.throws(
      () => signingMode(env),
      (error) => error.message.includes(omitted) && !error.message.includes('sensitive'),
    );
  }
});
