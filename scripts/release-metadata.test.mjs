import assert from 'node:assert/strict';
import test from 'node:test';

import { extractReleaseNotes, publicationFor } from './release-metadata.mjs';

test('extractReleaseNotes selects exactly the requested generated changelog section', () => {
  const changelog = `# cartograph

## 0.2.0

### Minor Changes

- Recover specifications.

## 0.1.1

### Patch Changes

- Fix a parser.
`;
  assert.equal(
    extractReleaseNotes(changelog, '0.2.0'),
    '### Minor Changes\n\n- Recover specifications.\n',
  );
  assert.equal(
    extractReleaseNotes(changelog, '0.1.1'),
    '### Patch Changes\n\n- Fix a parser.\n',
  );
});

test('extractReleaseNotes fails closed for missing or empty release notes', () => {
  assert.throws(() => extractReleaseNotes('# cartograph\n', '1.0.0'), /no ## 1\.0\.0 section/u);
  assert.throws(() => extractReleaseNotes('## 1.0.0\n\n## 0.9.0\nnotes\n', '1.0.0'), /is empty/u);
});

test('publicationFor distinguishes trusted releases from development prereleases', () => {
  assert.deepEqual(publicationFor('v0.3.0', 'signed'), {
    prerelease: 'false',
    title: 'Cartograph v0.3.0',
    version: '0.3.0',
  });
  assert.deepEqual(publicationFor('v0.3.0', 'unsigned-dev'), {
    prerelease: 'true',
    title: 'Cartograph v0.3.0 (unsigned development build)',
    version: '0.3.0',
  });
  assert.throws(() => publicationFor('0.3.0', 'signed'), /Invalid release tag/u);
  assert.throws(() => publicationFor('v0.3.0', 'partial'), /Invalid signing mode/u);
});
