#!/usr/bin/env node
// Help single-sourcing (#155, #470): `docs/help/*.md` is the one authored
// copy; the wiki mirrors it as `Help-<slug>` pages. The wiki follows main and
// never gates a PR: the PR gate validates the source locally, and a post-merge
// workflow (help-wiki-sync.yml) syncs the wiki from main.
//
// Usage:
//   node scripts/check-help-mirror.mjs --validate                # PR gate: no wiki needed
//   node scripts/check-help-mirror.mjs <path-to-wiki-clone>      # drift report
//   node scripts/check-help-mirror.mjs --sync <path-to-wiki-clone>  # write mirror
//
// `--sync` writes every `Help-<slug>.md` page and deletes mirrored pages whose
// source was removed, so the wiki converges on main after every merge.

import { readdirSync, readFileSync, existsSync, writeFileSync, unlinkSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const SLUG = /^[a-z0-9]+(?:-[a-z0-9]+)*$/u;
const BANNER =
  '<!-- Mirrored from docs/help/ by .github/workflows/help-wiki-sync.yml — edit docs/help/, not this page -->\n\n';

/** Read and validate `docs/help/*.md`; returns the pages the wiki mirrors. */
export function helpPages(helpDir) {
  const errors = [];
  const pages = [];
  if (!existsSync(helpDir)) return { pages, errors: [`${helpDir} does not exist`] };
  for (const file of readdirSync(helpDir).filter((name) => name.endsWith('.md')).sort()) {
    const slug = file.replace(/\.md$/u, '');
    if (!SLUG.test(slug)) {
      errors.push(`docs/help/${file}: slug must be lowercase kebab-case (${SLUG})`);
      continue;
    }
    const body = readFileSync(path.join(helpDir, file), 'utf8');
    if (body.trim() === '') {
      errors.push(`docs/help/${file} is empty`);
      continue;
    }
    pages.push({ file, name: `Help-${slug}.md`, content: BANNER + body });
  }
  if (pages.length === 0 && errors.length === 0) errors.push('docs/help/ has no topics');
  return { pages, errors };
}

/** Mirrored pages in the wiki clone (the only files `--sync` may delete). */
function mirroredPages(wikiDir) {
  return readdirSync(wikiDir).filter((name) => /^Help-.+\.md$/u.test(name));
}

export function driftErrors(pages, wikiDir) {
  const errors = [];
  const expected = new Set(pages.map((page) => page.name));
  for (const page of pages) {
    const wikiPage = path.join(wikiDir, page.name);
    if (!existsSync(wikiPage)) errors.push(`wiki page ${page.name} is missing`);
    else if (readFileSync(wikiPage, 'utf8') !== page.content) {
      errors.push(`wiki page ${page.name} differs from docs/help/${page.file}`);
    }
  }
  for (const name of mirroredPages(wikiDir)) {
    if (!expected.has(name)) errors.push(`wiki page ${name} has no docs/help/ source`);
  }
  return errors;
}

export function syncWiki(pages, wikiDir) {
  const expected = new Set(pages.map((page) => page.name));
  const changes = [];
  for (const page of pages) {
    const wikiPage = path.join(wikiDir, page.name);
    if (existsSync(wikiPage) && readFileSync(wikiPage, 'utf8') === page.content) continue;
    writeFileSync(wikiPage, page.content);
    changes.push(`synced ${page.name}`);
  }
  for (const name of mirroredPages(wikiDir)) {
    if (expected.has(name)) continue;
    unlinkSync(path.join(wikiDir, name));
    changes.push(`removed ${name}`);
  }
  return changes;
}

function main(args) {
  const mode = args[0] === '--sync' || args[0] === '--validate' ? args[0] : 'check';
  const wikiDir = mode === 'check' ? args[0] : args[1];
  if ((mode === '--validate') !== (wikiDir === undefined)) {
    console.error('usage: check-help-mirror.mjs --validate | [--sync] <path-to-wiki-clone>');
    return 2;
  }

  const { pages, errors } = helpPages(path.join(process.cwd(), 'docs', 'help'));
  if (errors.length > 0) {
    console.error(`Invalid docs/help (${errors.length}):\n  - ${errors.join('\n  - ')}`);
    return 1;
  }
  if (mode === '--validate') {
    console.log(`Help mirror source valid: ${pages.length} topics in docs/help/.`);
    return 0;
  }
  if (mode === '--sync') {
    const changes = syncWiki(pages, wikiDir);
    for (const change of changes) console.log(change);
    if (changes.length === 0) console.log('Wiki help mirror already matches docs/help/.');
    return 0;
  }
  const drift = driftErrors(pages, wikiDir);
  if (drift.length > 0) {
    console.error(`Help mirror drift (${drift.length}):\n  - ${drift.join('\n  - ')}`);
    console.error('The post-merge help-wiki-sync workflow repairs this from main.');
    return 1;
  }
  console.log('Help mirror check passed: docs/help/ and the wiki are identical.');
  return 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  process.exitCode = main(process.argv.slice(2));
}
