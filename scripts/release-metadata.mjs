#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { assertSemver } from './version.mjs';

const SCRIPT_PATH = fileURLToPath(import.meta.url);

export function extractReleaseNotes(changelog, version) {
  assertSemver(version);
  const lines = changelog.replaceAll('\r\n', '\n').split('\n');
  const heading = `## ${version}`;
  const start = lines.findIndex((line) => line.trim() === heading);
  if (start < 0) throw new Error(`CHANGELOG.md has no ${heading} section`);

  const next = lines.findIndex((line, index) => index > start && /^##\s+/u.test(line));
  const notes = lines.slice(start + 1, next < 0 ? lines.length : next).join('\n').trim();
  if (!notes) throw new Error(`CHANGELOG.md ${heading} section is empty`);
  return `${notes}\n`;
}

export function publicationFor(tag, signingMode) {
  if (!tag.startsWith('v')) throw new Error(`Invalid release tag: ${tag}`);
  const version = tag.slice(1);
  assertSemver(version);
  if (!['signed', 'unsigned-dev'].includes(signingMode)) {
    throw new Error(`Invalid signing mode: ${signingMode}`);
  }
  const signed = signingMode === 'signed';
  return {
    prerelease: String(!signed),
    title: signed ? `Cartograph ${tag}` : `Cartograph ${tag} (unsigned development build)`,
    version,
  };
}

function runCli() {
  const [command, value, extra, ...rest] = process.argv.slice(2);
  if (rest.length > 0) throw new Error('Too many arguments');

  if (command === 'metadata' && value && extra) {
    const metadata = publicationFor(value, extra);
    for (const [name, output] of Object.entries(metadata)) console.log(`${name}=${output}`);
    return;
  }
  if (command === 'notes' && value && extra) {
    process.stdout.write(extractReleaseNotes(readFileSync(extra, 'utf8'), value));
    return;
  }
  throw new Error(
    'Usage: node scripts/release-metadata.mjs <metadata TAG MODE|notes VERSION CHANGELOG_PATH>',
  );
}

if (process.argv[1] && path.resolve(process.argv[1]) === SCRIPT_PATH) {
  try {
    runCli();
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
