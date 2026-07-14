#!/usr/bin/env node

import { readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { assertSemver, checkVersions } from './version.mjs';

const SCRIPT_PATH = fileURLToPath(import.meta.url);
const DEFAULT_ROOT = path.resolve(path.dirname(SCRIPT_PATH), '..');

export function tagForVersion(version) {
  assertSemver(version);
  return `v${version}`;
}

export function releaseTag(root = DEFAULT_ROOT) {
  return tagForVersion(checkVersions(root));
}

export function pendingChangesets(root = DEFAULT_ROOT) {
  return readdirSync(path.join(root, '.changeset'))
    .filter((name) => name.endsWith('.md') && name !== 'README.md')
    .sort((left, right) => left.localeCompare(right));
}

function runCli() {
  const [command, ...extra] = process.argv.slice(2);
  if (extra.length > 0 || !['pending', 'tag'].includes(command)) {
    throw new Error('Usage: node scripts/release-version.mjs <pending|tag>');
  }
  if (command === 'tag') console.log(releaseTag());
  else console.log(pendingChangesets().join(','));
}

if (process.argv[1] && path.resolve(process.argv[1]) === SCRIPT_PATH) {
  try {
    runCli();
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
