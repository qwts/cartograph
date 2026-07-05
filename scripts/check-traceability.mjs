#!/usr/bin/env node
// Traceability gate: keeps docs/user_stories.md, docs/US-TM.md, and docs/adr/
// mutually consistent. Runs in CI on every PR (see .github/workflows/ci.yml).
//
// Checks:
//   1. Every US in user_stories.md has a row in the US-TM matrix, and vice versa.
//   2. AC ids are globally unique; every US defines at least one AC.
//   3. Each matrix row's AC range covers exactly the ACs its US defines.
//   4. Every ADR referenced in the matrix exists as a file in docs/adr/.
//   5. Every ADR file has a Status line, and appears in the US-TM ADR index.

import { readFile, readdir } from 'node:fs/promises';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const errors = [];
const fail = (msg) => errors.push(msg);

// --- user_stories.md ---------------------------------------------------------
const storiesSrc = await readFile(path.join(ROOT, 'docs/user_stories.md'), 'utf8');

/** @type {Map<string, Set<string>>} US id -> AC ids defined under it */
const storyAcs = new Map();
let currentUs = null;
const seenAcs = new Map(); // AC id -> owning US
for (const line of storiesSrc.split('\n')) {
  const usHeading = line.match(/^###\s+(US-\d{4})\s+—/u);
  if (usHeading) {
    currentUs = usHeading[1];
    if (storyAcs.has(currentUs)) fail(`Duplicate story heading ${currentUs} in user_stories.md`);
    storyAcs.set(currentUs, new Set());
    continue;
  }
  for (const ac of line.matchAll(/\*\*(AC-\d{4})\*\*/gu) ?? []) {
    if (!currentUs) {
      fail(`${ac[1]} appears before any US heading in user_stories.md`);
      continue;
    }
    if (seenAcs.has(ac[1])) fail(`${ac[1]} defined under both ${seenAcs.get(ac[1])} and ${currentUs}`);
    seenAcs.set(ac[1], currentUs);
    storyAcs.get(currentUs).add(ac[1]);
  }
}
for (const [us, acs] of storyAcs) {
  if (acs.size === 0) fail(`${us} defines no acceptance criteria`);
}

// --- US-TM.md ----------------------------------------------------------------
const matrixSrc = await readFile(path.join(ROOT, 'docs/US-TM.md'), 'utf8');

const expandAcRange = (cell) => {
  // "AC-0001..0003" or "AC-0013..0014" or a single "AC-0007"
  const acs = new Set();
  for (const m of cell.matchAll(/AC-(\d{4})(?:\.\.(\d{4}))?/gu)) {
    const start = Number(m[1]);
    const end = Number(m[2] ?? m[1]);
    for (let i = start; i <= end; i++) acs.add(`AC-${String(i).padStart(4, '0')}`);
  }
  return acs;
};

const matrixUs = new Map(); // US id -> { acs, adrs }
const matrixAdrRefs = new Set();
for (const line of matrixSrc.split('\n')) {
  const row = line.match(/^\|\s*(US-\d{4})\s*\|([^|]*)\|/u);
  if (!row) continue;
  const us = row[1];
  const adrs = new Set([...line.matchAll(/ADR-\d{4}/gu)].map((m) => m[0]));
  for (const adr of adrs) matrixAdrRefs.add(adr);
  matrixUs.set(us, { acs: expandAcRange(row[2]), adrs });
}

for (const us of storyAcs.keys()) {
  if (!matrixUs.has(us)) fail(`${us} exists in user_stories.md but has no row in US-TM.md`);
}
for (const us of matrixUs.keys()) {
  if (!storyAcs.has(us)) fail(`${us} has a row in US-TM.md but no story in user_stories.md`);
}
for (const [us, { acs }] of matrixUs) {
  const defined = storyAcs.get(us);
  if (!defined) continue;
  for (const ac of acs) {
    if (!defined.has(ac)) fail(`US-TM.md maps ${ac} to ${us}, but ${us} does not define it`);
  }
  for (const ac of defined) {
    if (!acs.has(ac)) fail(`${us} defines ${ac}, but the US-TM.md row does not cover it`);
  }
}

// --- docs/adr/ ---------------------------------------------------------------
const adrDir = path.join(ROOT, 'docs/adr');
const adrFiles = (await readdir(adrDir)).filter((f) => f.endsWith('.md'));
const adrIds = new Set();
for (const file of adrFiles) {
  const m = file.match(/^(ADR-\d{4})-[a-z0-9-]+\.md$/u);
  if (!m) {
    fail(`ADR file name "${file}" does not match ADR-NNNN-kebab-slug.md`);
    continue;
  }
  if (adrIds.has(m[1])) fail(`Duplicate ADR id ${m[1]} in docs/adr/`);
  adrIds.add(m[1]);
  const body = await readFile(path.join(adrDir, file), 'utf8');
  if (!/^-\s+\*\*Status:\*\*/mu.test(body)) fail(`${file} has no "- **Status:**" line`);
}

for (const adr of matrixAdrRefs) {
  if (!adrIds.has(adr)) fail(`US-TM.md references ${adr}, but docs/adr/ has no such file`);
}
// The ADR index table at the bottom of US-TM.md must list every ADR on disk.
const indexedAdrs = new Set([...matrixSrc.matchAll(/^\|\s*(ADR-\d{4})\s*\|/gmu)].map((m) => m[1]));
for (const adr of adrIds) {
  if (!indexedAdrs.has(adr)) fail(`${adr} exists in docs/adr/ but is missing from the US-TM.md ADR index`);
}
for (const adr of indexedAdrs) {
  if (!adrIds.has(adr)) fail(`US-TM.md ADR index lists ${adr}, but docs/adr/ has no such file`);
}

// --- verdict -----------------------------------------------------------------
if (errors.length > 0) {
  console.error(`Traceability check failed (${errors.length} error${errors.length === 1 ? '' : 's'}):\n`);
  for (const e of errors) console.error(`  ✗ ${e}`);
  process.exit(1);
}
console.log(
  `Traceability check passed: ${storyAcs.size} stories, ${seenAcs.size} ACs, ${adrIds.size} ADRs — consistent.`,
);
