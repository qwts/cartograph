import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');

test('version-cut preserves review, evidence, and immutable-tag gates', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/version-cut.yml'), 'utf8');
  assert.match(workflow, /npm run changeset:version/u);
  assert.doesNotMatch(workflow, /gh workflow run ci\.yml/u);
  assert.match(workflow, /event=push&head_sha=\$cut_commit/u);
  assert.match(workflow, /name == "CI" and \.conclusion == "success"/u);
  assert.match(workflow, /Manual recovery requires an existing \$tag tag/u);
  assert.match(workflow, /commits\/\$cut_commit\/pulls/u);
  assert.match(workflow, /\.head\.ref == "changeset-release\/main"/u);
  assert.match(workflow, /git tag -a "\$TAG" "\$CUT_COMMIT"/u);
  assert.match(workflow, /tagged_commit.*!=.*cut_commit/u);
  assert.match(workflow, /gh workflow run release\.yml --ref main -f tag="\$TAG"/u);
  assert.match(workflow, /actions\/create-github-app-token@[0-9a-f]{40}/u);
  assert.match(workflow, /secrets\.CHORES_DUMB_CLIENT_ID/u);
  assert.match(workflow, /secrets\.CHORES_DUMB_PRIVATE_KEY/u);
  assert.doesNotMatch(workflow, /RELEASE_TOKEN|secrets\.GITHUB_TOKEN/u);
});

test('CI enforces the governed lifecycle without draft jobs', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  assert.match(
    workflow,
    /^\s{2}pull_request:\n\s{4}branches: \[main\]\n\s{4}types: \[opened, synchronize, reopened, ready_for_review\]/mu,
  );
  assert.match(workflow, /^\s{2}merge_group:\n\s{4}types: \[checks_requested\]/mu);
  assert.match(workflow, /^\s{2}workflow_dispatch:/mu);
  assert.match(workflow, /exact-sha-preflight/u);
  assert.match(workflow, /format\('pr-\{0\}', github\.event\.pull_request\.number\)/u);
  assert.match(workflow, /cancel-in-progress: \$\{\{ github\.event_name != 'push' \}\}/u);
  assert.match(workflow, /github\.event\.pull_request\.draft == false/u);
  assert.match(
    workflow,
    /qwts\/playbook-engineering\/\.github\/actions\/ci-policy@a407b9afcfa8c515e3c4a41f535d1c5cfafa1116/u,
  );
  assert.match(workflow, /head_sha=\$TARGET_SHA/u);
  assert.match(workflow, /head_sha=\$GITHUB_SHA/u);
  assert.match(workflow, /name: Complete suite/u);
  assert.match(workflow, /name: CI/u);
});

test('direct non-CI entrypoints authorize before checkout and reusable calls inherit policy', () => {
  const workflows = [
    '.github/workflows/close-linked-issues.yml',
    '.github/workflows/package.yml',
    '.github/workflows/release.yml',
    '.github/workflows/version-cut.yml',
  ].map((file) => readFileSync(path.join(root, file), 'utf8'));
  for (const workflow of workflows) {
    assert.match(workflow, /ci-policy@a407b9afcfa8c515e3c4a41f535d1c5cfafa1116/u);
    assert.match(workflow, /authorization-only: 'true'/u);
    assert.match(workflow, /name: Action Policy/u);
    const policyPosition = workflow.indexOf('name: Action Policy');
    const checkoutPosition = workflow.indexOf('actions/checkout@');
    assert.ok(policyPosition >= 0 && policyPosition < checkoutPosition);
  }
  const packageWorkflow = workflows[1];
  assert.match(packageWorkflow, /if: github\.event_name != 'workflow_call'/u);
  assert.match(packageWorkflow, /github\.event_name == 'workflow_call' \|\| needs\.policy\.result == 'success'/u);
});

test('the immutable policy action contract covers both actor fields and fork refusal', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const guidance = readFileSync(path.join(root, 'AGENTS.md'), 'utf8');
  assert.match(workflow, /ci-policy@a407b9afcfa8c515e3c4a41f535d1c5cfafa1116/u);
  assert.match(guidance, /github\.triggering_actor/u);
  assert.match(guidance, /github\.actor/u);
  assert.match(guidance, /Public-fork workflows are\s+never approved or run/u);
});

test('complete suite retains every existing Cartograph gate', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const commands = [
    /node scripts\/check-traceability\.mjs/u,
    /npm run version:check/u,
    /npm run test:version/u,
    /cargo fmt --all --check/u,
    /cargo clippy --workspace --all-targets -- -D warnings/u,
    /cargo test --workspace/u,
    /cargo deny check/u,
    /npm run lint/u,
    /npm run typecheck/u,
    /npm run test/u,
    /npm run build/u,
  ];
  for (const command of commands) assert.match(workflow, command);
  for (const context of ['Docs & traceability', 'Rust', 'License & supply-chain', 'Frontend']) {
    assert.match(workflow, new RegExp(`name: ${context}`, 'u'));
  }
});

test('Advanced CodeQL is governed, immutable, and preserves Rust coverage', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/codeql.yml'), 'utf8');
  assert.match(workflow, /^\s{2}workflow_call:/mu);
  assert.doesNotMatch(workflow, /^\s{2}(push|pull_request|schedule):/mu);
  assert.match(workflow, /language: \[actions, javascript-typescript, rust\]/u);
  assert.match(
    workflow,
    /github\/codeql-action\/init@5595ccaf912efad79be6eef63a5619ff05969be3/u,
  );
  assert.match(
    workflow,
    /github\/codeql-action\/analyze@5595ccaf912efad79be6eef63a5619ff05969be3/u,
  );
  assert.match(workflow, /build-mode: none/u);
});

test('macOS packaging is exact-SHA, universal, fail-closed, and verified', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/package.yml'), 'utf8');

  assert.match(workflow, /name: Verify exact CI evidence/u);
  assert.match(workflow, /head_sha=\$PACKAGE_SHA/u);
  assert.match(workflow, /ref: \$\{\{ needs\.evidence\.outputs\.sha \}\}/u);
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
  assert.doesNotMatch(workflow, /Run required repository gates/u);
});

test('release publication is reviewed, exact-evidence-only, and idempotent', () => {
  const workflow = readFileSync(path.join(root, '.github/workflows/release.yml'), 'utf8');

  assert.match(workflow, /^\s{2}push:\n\s{4}tags:/mu);
  assert.match(workflow, /^\s{2}workflow_dispatch:/mu);
  assert.match(workflow, /group: release-\$\{\{ inputs\.tag \|\| github\.ref_name \}\}/u);
  assert.match(workflow, /cancel-in-progress: false/u);
  assert.match(workflow, /git cat-file -t "refs\/tags\/\$TAG"/u);
  assert.match(workflow, /expected=\$\(node scripts\/release-version\.mjs tag\)/u);
  assert.match(workflow, /commits\/\$tag_commit\/pulls/u);
  assert.match(workflow, /\.head\.ref == "changeset-release\/main"/u);
  assert.match(workflow, /event=merge_group&head_sha=\$tag_commit/u);
  assert.match(workflow, /name == "Complete suite" and \.conclusion == "success"/u);
  assert.match(workflow, /event=pull_request&head_sha=\$pr_head/u);
  assert.match(workflow, /uses: \.\/\.github\/workflows\/package\.yml/u);
  assert.match(workflow, /ref: \$\{\{ needs\.validate\.outputs\.tag \}\}/u);
  assert.match(workflow, /name: \$\{\{ needs\.build\.outputs\.artifact_name \}\}/u);
  assert.match(workflow, /SIGNING_MODE: \$\{\{ needs\.build\.outputs\.signing_mode \}\}/u);
  assert.match(workflow, /release-metadata\.mjs notes "\$VERSION" CHANGELOG\.md/u);
  assert.match(workflow, /gh release edit "\$TAG"/u);
  assert.match(workflow, /gh release delete-asset/u);
  assert.match(workflow, /gh release upload "\$TAG" dist\/\* --clobber/u);
  assert.match(workflow, /permissions:\n\s{6}actions: read\n\s{6}contents: read/u);
  assert.match(workflow, /actions\/create-github-app-token@[0-9a-f]{40}/u);
  assert.match(workflow, /secrets\.CHORES_DUMB_CLIENT_ID/u);
  assert.match(workflow, /secrets\.CHORES_DUMB_PRIVATE_KEY/u);
  const publish = workflow.split('- name: Create or update release without duplicate assets')[1] ?? '';
  assert.doesNotMatch(workflow, /RELEASE_TOKEN/u);
  assert.doesNotMatch(publish, /GH_TOKEN: \$\{\{ github\.token \}\}/u);
});
