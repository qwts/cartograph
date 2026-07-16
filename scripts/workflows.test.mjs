import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');

test('version-cut preserves review, token-trigger, and immutable-tag gates', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/version-cut.yml'), 'utf8');
  assert.match(workflow, /changesets\/action@63a615b9cd06ba9a3e6d13796c7fbcb080a60a0b/u);
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

test('release publication is reviewed-tag-only, exact-artifact, and idempotent', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/release.yml'), 'utf8');

  assert.match(workflow, /^\s{2}push:\n\s{4}tags:/mu);
  assert.match(workflow, /^\s{2}workflow_dispatch:/mu);
  assert.match(workflow, /group: release-\$\{\{ inputs\.tag \|\| github\.ref_name \}\}/u);
  assert.match(workflow, /cancel-in-progress: false/u);
  assert.match(workflow, /git cat-file -t "refs\/tags\/\$TAG"/u);
  assert.match(workflow, /expected=\$\(node scripts\/release-version\.mjs tag\)/u);
  assert.match(workflow, /commits\/\$tag_commit\/pulls/u);
  assert.match(workflow, /\.head\.ref == "changeset-release\/main"/u);
  assert.match(workflow, /uses: \.\/\.github\/workflows\/package\.yml/u);
  assert.match(workflow, /ref: \$\{\{ needs\.validate\.outputs\.tag \}\}/u);
  assert.match(workflow, /name: \$\{\{ needs\.build\.outputs\.artifact_name \}\}/u);
  assert.match(workflow, /SIGNING_MODE: \$\{\{ needs\.build\.outputs\.signing_mode \}\}/u);
  assert.match(workflow, /release-metadata\.mjs notes "\$VERSION" CHANGELOG\.md/u);
  assert.match(workflow, /gh release edit "\$TAG"/u);
  assert.match(workflow, /gh release delete-asset/u);
  assert.match(workflow, /gh release upload "\$TAG" dist\/\* --clobber/u);
  assert.match(workflow, /permissions:\n\s{6}contents: write/u);
});
