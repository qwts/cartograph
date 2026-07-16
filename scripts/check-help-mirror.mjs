#!/usr/bin/env node
// Help single-sourcing gate (#155): `docs/help/*.md` is the one authored
// copy; the wiki mirrors it as `Help-<slug>` pages. This check fails when
// the two diverge so in-app Help and the wiki can never drift.
//
// Usage:
//   node scripts/check-help-mirror.mjs <path-to-wiki-clone>
//   node scripts/check-help-mirror.mjs --sync <path-to-wiki-clone>  # write mirror
//
// CI clones the public wiki (https://github.com/qwts/cartograph.wiki.git)
// and runs the check; `--sync` regenerates the mirror pages locally so the
// fix for drift is always "run sync, push the wiki".

import { readdirSync, readFileSync, existsSync, writeFileSync } from 'node:fs';
import path from 'node:path';

const args = process.argv.slice(2);
const sync = args[0] === '--sync';
const wikiDir = sync ? args[1] : args[0];
if (!wikiDir) {
  console.error('usage: check-help-mirror.mjs [--sync] <path-to-wiki-clone>');
  process.exit(2);
}

const helpDir = path.join(process.cwd(), 'docs', 'help');
const banner =
  '<!-- Mirrored from docs/help/ — edit there and run scripts/check-help-mirror.mjs --sync -->\n\n';

const errors = [];
for (const file of readdirSync(helpDir).filter((name) => name.endsWith('.md')).sort()) {
  const slug = file.replace(/\.md$/u, '');
  const source = banner + readFileSync(path.join(helpDir, file), 'utf8');
  const wikiPage = path.join(wikiDir, `Help-${slug}.md`);
  if (sync) {
    writeFileSync(wikiPage, source);
    console.log(`synced Help-${slug}.md`);
    continue;
  }
  if (!existsSync(wikiPage)) {
    errors.push(`wiki page Help-${slug}.md is missing`);
    continue;
  }
  if (readFileSync(wikiPage, 'utf8') !== source) {
    errors.push(`wiki page Help-${slug}.md differs from docs/help/${file}`);
  }
}

if (errors.length > 0) {
  console.error(`Help mirror drift (${errors.length}):\n  - ${errors.join('\n  - ')}`);
  console.error('Fix: node scripts/check-help-mirror.mjs --sync <wiki-clone> and push the wiki.');
  process.exit(1);
}
if (!sync) console.log('Help mirror check passed: docs/help/ and the wiki are identical.');
