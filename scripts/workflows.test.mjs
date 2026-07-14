import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');

test('version-cut preserves review, token-trigger, and immutable-tag gates', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/version-cut.yml'), 'utf8');
  assert.match(workflow, /changesets\/action@d94a5c301145045a0960133674e003b265942a22/u);
  assert.match(workflow, /version: npm run changeset:version/u);
  assert.match(workflow, /gh workflow run ci\.yml --ref changeset-release\/main/u);
  assert.match(workflow, /git log --first-parent --format=%H -- package\.json/u);
  assert.match(workflow, /git tag -a "\$tag" "\$cut_commit"/u);
  assert.match(workflow, /tagged_commit.*!=.*cut_commit/u);
  assert.match(workflow, /gh workflow run release\.yml --ref main -f tag="\$tag"/u);
});

test('CI accepts explicit dispatches for bot-refreshed version branches', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  assert.match(workflow, /^\s{2}workflow_dispatch:/mu);
});
