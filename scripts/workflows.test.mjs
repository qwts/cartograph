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
  assert.match(workflow, /Manual recovery requires an existing \$tag tag/u);
  assert.match(workflow, /commits\/\$cut_commit\/pulls/u);
  assert.match(workflow, /\.head\.ref == "changeset-release\/main"/u);
  assert.match(workflow, /git tag -a "\$tag" "\$cut_commit"/u);
  assert.match(workflow, /tagged_commit.*!=.*cut_commit/u);
  assert.match(workflow, /gh workflow run release\.yml --ref main -f tag="\$tag"/u);
});

test('CI accepts explicit dispatches for bot-refreshed version branches', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  assert.match(workflow, /^\s{2}workflow_dispatch:/mu);
});

test('macOS packaging is exact-ref, universal, fail-closed, and verified', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/package.yml'), 'utf8');

  assert.match(workflow, /ref: \$\{\{ inputs\.ref \|\| github\.ref \}\}/u);
  assert.match(workflow, /node scripts\/signing-secrets\.mjs/u);
  assert.match(workflow, /APPLE_CERTIFICATE: \$\{\{ secrets\.CSC_LINK \}\}/u);
  assert.match(workflow, /APPLE_CERTIFICATE_PASSWORD: \$\{\{ secrets\.CSC_KEY_PASSWORD \}\}/u);
  assert.match(workflow, /APPLE_API_KEY_PATH/u);
  assert.match(workflow, /APPLE_API_KEY: \$\{\{ secrets\.APPLE_API_KEY_ID \}\}/u);
  assert.match(workflow, /APPLE_API_ISSUER: \$\{\{ secrets\.APPLE_API_ISSUER \}\}/u);
  assert.match(workflow, /APPLE_SIGNING_IDENTITY: '-'/u);
  assert.match(workflow, /universal-apple-darwin/u);
  assert.match(workflow, /codesign --verify --deep --strict/u);
  assert.match(workflow, /spctl --assess/u);
  assert.match(workflow, /xcrun notarytool submit/u);
  assert.match(workflow, /xcrun stapler staple/u);
  assert.match(workflow, /xcrun stapler validate/u);
  assert.match(workflow, /Cartograph_\$\{version\}_universal_\$\{SIGNING_MODE\}/u);
  assert.match(workflow, /mkdir -p ui\/dist/u);
  assert.match(workflow, /node scripts\/check-traceability\.mjs/u);
  assert.match(workflow, /cargo clippy --workspace --all-targets -- -D warnings/u);
  assert.match(workflow, /npm --prefix ui run build/u);
});
